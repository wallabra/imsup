use std::collections::HashSet;
use std::fs::{exists, File};
use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use get_if_addrs::{
    get_if_addrs,
    IfAddr::{V4, V6},
};
use ipnetwork::IpNetwork;
use log::{debug, error, info, warn};
use scraper::{ElementRef, Html, Selector};
use serde_derive::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

fn href_hp(addr: &str, path: &str) -> String {
    format!("http://{}/hp/device/{}", addr, path)
}

fn href_supplies_page(addr: &str) -> String {
    href_hp(addr, "InternalPages/Index?id=SuppliesStatus")
}

fn href_device_info(addr: &str) -> String {
    href_hp(addr, "DeviceInformation/View")
}

#[derive(Serialize, Deserialize)]
struct DeviceInfo {
    pub location: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub name: Option<String>,
    pub nick: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SuppliesCartridge {
    pub estim_pages_remaining: Option<u32>,
    pub printed: Option<u32>,
    pub ink_level: Option<u16>,
}

#[derive(Serialize, Deserialize)]
struct Supplies {
    pub c: Option<SuppliesCartridge>,
    pub m: Option<SuppliesCartridge>,
    pub y: Option<SuppliesCartridge>,
    pub k: Option<SuppliesCartridge>,
}

#[derive(Serialize, Deserialize)]
struct PrinterInfo {
    pub addr: String,
    pub info: DeviceInfo,
    pub supplies: Supplies,
}

async fn get_document_at_href(client: &reqwest::Client, href: String) -> Result<Html, String> {
    let req = match client.get(href).send().await {
        Ok(text) => text,
        Err(err) => return Err(format!("{}", err)),
    };
    match req.text().await {
        Ok(text) => Ok(Html::parse_document(&text)),
        Err(err) => Err(format!("{}", err)),
    }
}

fn get_cartridge_block<'a, 'h: 'a>(frag: &'h Html, colname: &str) -> Option<ElementRef<'a>> {
    frag.select(&Selector::parse(&format!(".consumable-block-{}", colname)).unwrap())
        .next()
}

fn get_cartridge_row(block: &ElementRef<'_>, row_name: &str) -> Option<String> {
    block
        .select(&Selector::parse(&format!("[id$=\"{}\"]", row_name)).unwrap())
        .next()
        .map(|r| r.text().collect::<String>())
}

fn scrape_cartridge_color(colname: &str, document: &Html) -> Option<SuppliesCartridge> {
    let block = get_cartridge_block(document, colname);
    block.map(|block| SuppliesCartridge {
        estim_pages_remaining: get_cartridge_row(&block, "EstimatedPagesRemaining").and_then(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok()
        }),
        printed: get_cartridge_row(&block, "PagesPrintedWithSupply").map(|s| s.parse().unwrap()),
        ink_level: (block
            .select(&Selector::parse(".consumable-block-header .data.percentage").unwrap())
            .next()
            .map(|s| {
                (s.text()
                    .collect::<String>()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u32>()
                    .unwrap()
                    * u16::MAX as u32
                    / 100) as u16
            })),
    })
}

fn scrape_supplies_info(document: Html) -> Supplies {
    Supplies {
        c: scrape_cartridge_color("cyan", &document),
        m: scrape_cartridge_color("magenta", &document),
        y: scrape_cartridge_color("yellow", &document),
        k: scrape_cartridge_color("black", &document),
    }
}

fn get_p_info(doc: &Html, infoname: &str) -> Option<String> {
    Some(
        doc.select(&Selector::parse(&format!("#{}", &infoname)).unwrap())
            .next()?
            .text()
            .collect::<String>(),
    )
}

fn scrape_device_info(document: Html) -> DeviceInfo {
    DeviceInfo {
        location: get_p_info(&document, "DeviceLocation"),
        model: get_p_info(&document, "DeviceModel"),
        name: get_p_info(&document, "ProductName"),
        nick: get_p_info(&document, "DeviceName"),
        serial: get_p_info(&document, "DeviceSerialNumber"),
    }
}

async fn get_supplies_info(client: reqwest::Client, addr: String) -> Result<Supplies, String> {
    let href = href_supplies_page(&addr);
    let document = get_document_at_href(&client, href.clone()).await?;
    info!("Got supplies info page for {}: {}", addr, href);
    Ok(scrape_supplies_info(document))
}

async fn get_device_info(client: reqwest::Client, addr: String) -> Result<DeviceInfo, String> {
    let href = href_device_info(&addr);
    let document = get_document_at_href(&client, href.clone()).await?;
    info!("Got device info page for {}: {}", addr, href);
    Ok(scrape_device_info(document))
}

async fn process_addr<'a>(addr: String) -> Result<PrinterInfo, String> {
    info!("Contacting printer: {}", addr);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let device_info = get_device_info(client.clone(), addr.clone());
    let supplies_info = get_supplies_info(client.clone(), addr.clone());

    Ok(PrinterInfo {
        addr,
        info: device_info.await?,
        supplies: supplies_info.await?,
    })
}

async fn prod_addr_for_printer(own_tx: mpsc::Sender<String>, target: IpAddr) {
    tokio::spawn(async move {
        let data = [1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 0];
        let timeout = Duration::from_secs(2);
        //debug!("Pinging address {}..", addr);
        if let Ok(reply) = ping_rs::send_ping_async(&target, timeout, Arc::new(&data), None).await {
            let saddr = target.to_string();
            let href = href_hp(&saddr, "DeviceStatus/Index");

            debug!(
                "Prodding address {} (on {}) to check if is compatible printer.. ({:?})",
                target, href, reply
            );

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap();

            if client.get(href).send().await.is_ok() {
                info!("Found printer: {}", target);
                if let Err(err_addr) = own_tx.send(saddr).await {
                    error!("Async receiver dropped before printer address could be sent from contact thread: {}", err_addr)
                }
            };
        }
    });
}

async fn broadcast_ping_printers(tx: mpsc::Sender<String>) {
    let mut tried: HashSet<IpAddr> = HashSet::new();

    let ifaces = get_if_addrs();

    if let Ok(ifaces) = ifaces {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            info!("Pinging addresses on network interface {}", iface.name);

            let ip = iface.ip();
            let ifaddr = iface.addr;

            if tried.contains(&ip) {
                continue;
            }
            tried.insert(ip);

            let network = match ifaddr {
                V4(addr) => IpNetwork::with_netmask(ip, IpAddr::V4(addr.netmask)),
                V6(addr) => IpNetwork::with_netmask(ip, IpAddr::V6(addr.netmask)),
            };

            if let Ok(network) = network {
                for target in &network {
                    let tip = target;

                    if tip == ip {
                        continue;
                    }

                    let own_tx = tx.clone();
                    prod_addr_for_printer(own_tx, tip).await;
                }
            }
        }
    }
}

const IP_FNAME: &str = "impressoras.txt";
const OUT_FNAME: &str = "relatorio_impressoras.json";

async fn get_ips_from_file(tx: mpsc::Sender<String>) {
    match exists(IP_FNAME) {
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

    let ip_fopen = File::open(IP_FNAME);
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

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let (tx, mut rx) = mpsc::channel(100);
    let mut aggregate: Vec<PrinterInfo> = vec![];
    let mut tasks: Vec<(String, JoinHandle<Result<PrinterInfo, String>>)> = vec![];

    broadcast_ping_printers(tx.clone()).await;
    get_ips_from_file(tx.clone()).await;
    drop(tx);

    let mut checked: HashSet<String> = HashSet::new();

    while let Some(addr) = rx.recv().await {
        if checked.contains(&addr) {
            info!("Skipping duplicate address: {}", addr);
            continue;
        }
        checked.insert(addr.clone());
        let task = tokio::spawn(process_addr(addr.to_owned()));
        tasks.push((addr.to_owned(), task));
    }

    for (addr, result) in
        futures::future::join_all(tasks.into_iter().map(|(addr, t)| async { (addr, t.await) }))
            .await
    {
        match result {
            Ok(r_info) => match r_info {
                Ok(info) => aggregate.push(info),
                Err(err) => {
                    error!("Error reading printer info from address {}: {}", addr, err)
                }
            },
            Err(err) => {
                error!("Error reading printer info from address {}: {}", addr, err)
            }
        }
    }

    if aggregate.is_empty() {
        warn!("No printer information obtained, aborting");
        return;
    }

    match serde_json::to_string(&aggregate) {
        Ok(value) => {
            println!("{}", value);
            if let Ok(mut file) = File::create(OUT_FNAME) {
                if let Err(err) = file.write_all(value.as_bytes()) {
                    error!("Error writing JSON to output file {:?}: {}", OUT_FNAME, err);
                }
            }
        }
        Err(err) => error!("Error producing JSON for aggregate printer info: {}", err),
    }
}
