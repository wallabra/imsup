use std::collections::HashSet;
use std::fs::{exists, File};
use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;

use clap::{Parser, ValueEnum};
use futures::{stream, StreamExt};
use impsupri::contact::process_addr;
use impsupri::printerinfo::PrinterInfo;
use impsupri::scan::broadcast_ping_printers;
use log::{error, info, warn};
use tokio::sync::mpsc;

/// Network printer scanning mode.
#[derive(ValueEnum, PartialEq, Debug, Clone, Copy, Default)]
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
    #[default]
    Cluster,
}

/// Output mode.
#[derive(ValueEnum, PartialEq, Debug, Clone, Copy, Default)]
enum OutputMode {
    /// No output.
    #[value()]
    None,

    /// JSON output.
    #[value()]
    Json,

    /// The # of pages printed per color cartridge per printer.
    /// Produces a CSV with 5 columns:
    ///   serial  c  m  y  k
    ///
    /// Mono printers will have empty columns for all colors, except key
    /// (black).
    #[value(name = "printed_values")]
    #[default]
    PrintedPages,
}

/// Scans for HP printers, from a list of IPs and from the network, and
/// scrapes their internal webpages for information, producing a JSON report
/// that can be written to stdout or to a file.
#[derive(Parser)]
struct Args {
    /// Disables reading from an IP list file.
    #[arg(short, long)]
    disable_ip_list: bool,

    /// File from which to read a list of IPs to always contact
    ///
    /// These IPs are processed independently from the scanner's, but no same
    /// IP is contacted twice.
    ///
    /// Defaults to "impressoras.txt" for historical reasons.
    #[arg(short, long)]
    ip_list: Option<String>,

    /// Sets the network scanning mode.
    ///
    /// Defaults to 'cluster'.
    #[arg(value_enum, short, long)]
    scan_mode: Option<ScanMode>,

    /// Disables writing the JSON to stdout.
    #[arg(short, long)]
    writeless: bool,

    /// Disables writing the JSON to a file.
    #[arg(short, long)]
    no_out: bool,

    /// Output filename of JSON report.
    ///
    /// Defaults to "relatorio_impressoras.<ext>" for historical reasons.
    #[arg(short, long)]
    out: Option<String>,

    /// Disable non-logger status prints.
    #[arg(short, long)]
    quiet: Option<String>,

    /// Number of simultaneous concurrent printer scraper tasks to run.
    ///
    /// The higher, the faster the scanner printers list will be processed,
    /// but the higher the chance of a contact failing due to overload.
    ///
    /// The default is 4.
    #[arg(long, short = 't')]
    contact_parallel: Option<usize>,

    /// Number of simultaneous ping tasks to run.
    ///
    /// The default is 128.
    #[arg(long, short)]
    ping_parallel: Option<usize>,

    /// Sets the kind of report to generate.
    ///
    /// Defaults to 'printed_pages' for historical reasons.
    #[arg(value_enum, short = 'm', long)]
    output_mode: Option<OutputMode>,
}

async fn get_ips_from_file(fname: &str, tx: mpsc::Sender<String>) {
    match exists(fname) {
        Ok(existence) => {
            if !existence {
                info!("Address list file {:?} not found, skipping it", fname);
                return;
            }
        }
        Err(err) => {
            error!(
                "Could not verify the existence of address list file {:?}: {}",
                fname, err
            );
            return;
        }
    }

    info!("Address list file {:?} found, processing IPs...", fname);

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
                            warn!("Line from address list file {:?} (line {}) could not be parsed as an IP address: {}", fname, linenum + 1, line);
                            continue;
                        }
                        info!("Found listed printer address to check: {}", line);
                        if let Err(line) = tx.send(line).await {
                            error!("Async receiver dropped before addresses from address list file {:?} could be sent: {}", fname, line);
                            break;
                        }
                    }
                    Err(err) => {
                        error!("Could not read line from IP list file {:?}: {}", fname, err);
                    }
                }
            }
        }
        Err(fopen_err) => {
            error!(
                "Couldn't open existing address list file {:?}: {}",
                fname, fopen_err
            );
        }
    }
}

const IP_DEFAULT_FNAME: &str = "impressoras.txt";
const OUT_DEFAULT_FNAME: &str = "relatorio_impressoras";

#[tokio::main]
async fn main() {
    let args = Args::parse();

    pretty_env_logger::init();
    eprintln!("Welcome to Printer Scanner.");

    // Find printers to contact.
    //
    // Relevant addresses will be received through the
    // [tokio::mpsc::channel] below.
    eprintln!("Finding printers...");

    let (tx, mut rx) = mpsc::channel(100);

    // File list
    if !args.disable_ip_list {
        get_ips_from_file(
            &args.ip_list.unwrap_or(IP_DEFAULT_FNAME.to_owned()),
            tx.clone(),
        )
        .await;
    }

    // Network scan
    if args.scan_mode.unwrap_or_default() != ScanMode::None {
        let num_found = broadcast_ping_printers(
            tx.clone(),
            args.scan_mode.unwrap_or_default() == ScanMode::Cluster,
            args.ping_parallel.unwrap_or(128),
        )
        .await;
        info!(
            "Automatically found {} printers to process in the network",
            num_found
        );
    }

    drop(tx);

    // Contact printers.
    //
    // Contact tasks will be generated and sent through
    // the [tokio::mpsc::unbounded_channel] below.
    //
    // This way they can be done in batches using
    // [futures::StreamExt::buffer_unordered].
    let mut checked: HashSet<String> = HashSet::new();

    let (task_tx, mut task_rx) = mpsc::unbounded_channel();

    // Generate contact tasks
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

    // Execute contact tasks
    eprintln!("Contacting printers...");
    let task_stream = stream::poll_fn(|c| task_rx.poll_recv(c));

    let aggregate: Vec<PrinterInfo> = task_stream
        .buffer_unordered(args.contact_parallel.unwrap_or(4))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();

    if aggregate.is_empty() {
        error!("No printer information obtained, aborting");
        return;
    }

    // Generate output
    let output_mode = args.output_mode.unwrap_or_default();

    let value = match output_mode {
        OutputMode::Json => {
            eprintln!("Generating output JSON...");
            match serde_json::to_string(&aggregate) {
                Ok(value) => value,
                Err(err) => {
                    error!("Error producing JSON for aggregate printer info: {}", err);
                    return;
                }
            }
        }
        OutputMode::PrintedPages => {
            eprintln!("Return printed pages table...");
            String::from("\"serial\",\"c\",\"m\",\"y\",\"k\"\n")
                + &aggregate
                    .iter()
                    .filter_map(|pi: &PrinterInfo| {
                        pi.info.serial.as_ref().map(|serial| {
                            format!(
                                "\"{}\",{}",
                                serial,
                                pi.supplies
                                    .as_vec()
                                    .iter()
                                    .map(|cart| match cart {
                                        None => "\"\"".to_owned(),
                                        Some(val) =>
                                            "\"".to_owned()
                                                + &val
                                                    .printed
                                                    .map_or("".to_owned(), |v| v.to_string())
                                                + "\"",
                                    })
                                    .collect::<Vec<_>>()
                                    .join(",")
                            )
                        })
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
        }
        OutputMode::None => {
            return;
        }
    };

    // Write output
    eprintln!("Writing to file and stdout...");

    if !args.writeless {
        println!("{}", value);
    }
    if !args.no_out {
        let out_fname = args.out.unwrap_or(format!(
            "{}.{}",
            OUT_DEFAULT_FNAME,
            match output_mode {
                OutputMode::None => "",
                OutputMode::Json => "json",
                OutputMode::PrintedPages => "csv",
            }
        ));
        if let Ok(mut file) = File::create(out_fname.clone()) {
            if let Err(err) = file.write_all(value.as_bytes()) {
                error!("Error writing JSON to output file {:?}: {}", out_fname, err);
            }
        }
    }
}
