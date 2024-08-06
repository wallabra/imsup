use std::collections::HashSet;
use std::fs::{exists, File};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use futures::future::join_all;
use futures::{stream, StreamExt};
use get_if_addrs::{
    get_if_addrs,
    IfAddr::{V4, V6},
};
use ipnetwork::{IpNetwork, Ipv4Network};
use log::{debug, error, info, warn};
use scraper::selectable::Selectable;
use scraper::{ElementRef, Html, Selector};
use serde_derive::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Network printer scanning mode.
#[derive(ValueEnum)]
enum ScanMode {
    /// Skips scanning for printers in the network.
    #[value()]
    None,

    /// Scans the current network for printers.
    #[value()]
    Network,

    /// Scans the current network, and all neighboring networks, for printers.
    ///
    /// This is always skipped if the current network is bigger than a /24, for
    /// scalability and security reasons.
    #[value()]
    Cluster,
}

impl Default for ScanMode {
    fn default() -> Self {
        return ScanMode::Cluster;
    }
}

/// Scans for HP printers, from a list of IPs and from the network, and
/// scrapes their internal webpages for information, producing a JSON report
/// that can be written to stdout or to a file.
#[derive(Parser)]
struct Args {
    /// Disables reading from an IP list file.
    #[arg(short, long)]
    no_ip_list: bool,

    /// File from which to read a list of IPs to always contact
    ///
    /// These IPs are processed independently from the scanner's, but no same
    /// IP is contacted twice.
    #[arg(short, long)]
    ip_list: Option<String>,

    /// Sets the network scanning mode.
    #[arg(value_enum, short, long)]
    scan_mode: Option<ScanMode>,

    /// Disables writing the JSON to stdout.
    #[arg(short, long)]
    writeless: bool,

    /// Disables writing the JSON to a file.
    #[arg(short, long)]
    no_out: bool,

    /// Output filename of JSON report.
    #[arg(short, long)]
    out: Option<String>,

    /// Disable non-logger status prints.
    #[arg(short, long)]
    quiet: Option<String>,
}

async fn get_ips_from_file(fname: &str, tx: mpsc::Sender<String>) {
    match exists(fname) {
        Ok(existence) => {
            if !existence {
                info!("Address list file {:?} not found, skipping it", IP_FNAME);
                return;
            }
        }
        Err(err) => {
            error!(
                "Could not verify the existence of address list file {:?}: {}",
                IP_FNAME, err
            );
            return;
        }
    }

    info!("Address list file {:?} found, processing IPs...", IP_FNAME);

    let ip_fopen = File::open(fname);
    match ip_fopen {
        Ok(ip_file) => {
            let reader = BufReader::new(ip_file);
            for (linenum, lineread) in reader.lines().enumerate() {
                match lineread {
                    Ok(line) => {
                        if line.starts_with('#')
                            || line
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join("")
                                .is_empty()
                        {
                            // is a comment, skip
                            continue;
                        }
                        if line.parse::<IpAddr>().is_err() {
                            warn!("Line from address list file {:?} (line {}) could not be parsed as an IP address: {}", IP_FNAME, linenum + 1, line);
                            continue;
                        }
                        info!("Found listed printer address to check: {}", line);
                        if let Err(line) = tx.send(line).await {
                            error!("Async receiver dropped before addresses from address list file {:?} could be sent: {}", IP_FNAME, line);
                            break;
                        }
                    }
                    Err(err) => {
                        error!(
                            "Could not read line from IP list file {:?}: {}",
                            IP_FNAME, err
                        );
                    }
                }
            }
        }
        Err(fopen_err) => {
            error!(
                "Couldn't open existing address list file {:?}: {}",
                IP_FNAME, fopen_err
            );
        }
    }
}

const IP_DEFAULT_NAME: &str = "impressoras.txt";
const OUT_DEFAULT_NAME: &str = "relatorio_impressoras.json";

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    eprintln!("Welcome to Printer Scanner.");

    let args = Args::parse();

    eprintln!("Finding printers...");

    let (tx, mut rx) = mpsc::channel(100);

    if args.scan_mode.unwrap_or_default() != ScanMode::None {
        let num_found =
            broadcast_ping_printers(tx.clone(), args.scan_mode == ScanMode::Cluster).await;
        info!(
            "Automatically found {} printers to process in the network",
            num_found
        );
    }

    if !args.no_ip_list {
        get_ips_from_file(
            &args.ip_list.unwrap_or(IP_DEFAULT_FNAME.to_owned()),
            tx.clone(),
        )
        .await;
    }
    drop(tx);

    let mut checked: HashSet<String> = HashSet::new();

    let (task_tx, mut task_rx) = mpsc::unbounded_channel();

    while let Some(addr) = rx.recv().await {
        if checked.contains(&addr) {
            info!("Skipping duplicate address: {}", addr);
            continue;
        }
        checked.insert(addr.clone());
        let task_addr = addr.clone();
        let task = async move {
            match process_addr(task_addr.clone()).await {
                Ok(info) => Some(info),
                Err(err) => {
                    warn!(
                        "Error reading printer info from address {}: {}",
                        task_addr, err
                    );
                    None
                }
            }
        };
        if let Err(err) = task_tx.send(task) {
            error!(
                "Error sending task to process printer at address {}: {}",
                addr, err
            );
        }
    }

    drop(task_tx);
    eprintln!("Contacting printers...");
    let task_stream = stream::poll_fn(|c| task_rx.poll_recv(c));

    let aggregate: Vec<PrinterInfo> = task_stream
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();

    if aggregate.is_empty() {
        error!("No printer information obtained, aborting");
        return;
    }

    eprintln!("Serializing JSON and writing to file and stdout...");
    match serde_json::to_string(&aggregate) {
        Ok(value) => {
            if !args.writeless {
                println!("{}", value);
            }
            if !args.no_out {
                if let Ok(mut file) = File::create(args.out.unwrap_or(OUT_DEFAULT_NAME.to_owned()))
                {
                    if let Err(err) = file.write_all(value.as_bytes()) {
                        error!("Error writing JSON to output file {:?}: {}", OUT_FNAME, err);
                    }
                }
            }
        }
        Err(err) => error!("Error producing JSON for aggregate printer info: {}", err),
    }
}
