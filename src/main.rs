#![feature(is_none_or)]
use std::collections::HashSet;
use std::fs::{exists, File};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

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

impl DeviceInfo {
    pub fn all_null(&self) -> bool {
        self.location.is_none()
            && self.model.is_none()
            && self.serial.is_none()
            && self.name.is_none()
            && self.nick.is_none()
    }
}

#[derive(Serialize, Deserialize)]
struct SuppliesCartridge {
    pub estim_pages_remaining: Option<u32>,
    pub printed: Option<u32>,
    pub ink_level: Option<u16>,
}

impl SuppliesCartridge {
    pub fn all_null(&self) -> bool {
        self.estim_pages_remaining.is_none() && self.printed.is_none() && self.ink_level.is_none()
    }
}

#[derive(Serialize, Deserialize)]
struct Supplies {
    pub c: Option<SuppliesCartridge>,
    pub m: Option<SuppliesCartridge>,
    pub y: Option<SuppliesCartridge>,
    pub k: Option<SuppliesCartridge>,
}

fn null_option(cartridge: &Option<SuppliesCartridge>) -> bool {
    cartridge.as_ref().is_none_or(|c| c.all_null())
}

impl Supplies {
    pub fn all_null(&self) -> bool {
        null_option(&self.c) && null_option(&self.m) && null_option(&self.y) && null_option(&self.k)
    }
}

#[derive(Serialize, Deserialize)]
struct PrinterInfo {
    pub addr: String,
    pub info: DeviceInfo,
    pub supplies: Supplies,
}

impl PrinterInfo {
    pub fn all_null(&self) -> bool {
        self.info.all_null() && self.supplies.all_null()
    }
}

async fn get_document_at_href(client: &reqwest::Client, href: String) -> Result<Html, String> {
    let req = match client.get(href).send().await {
        Ok(text) => text,
        Err(err) => return Err(format!("{}", err)),
    };
    let html = match req.text().await {
        Ok(text) => Ok(Html::parse_document(&text)),
        Err(err) => Err(format!("{}", err)),
    };
    html.and_then(|html| {
        if is_login_screen(&html) {
            Err("Hit a login screen".to_owned())
        } else {
            Ok(html)
        }
    })
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
            .select(&Selector::parse(".consumable-block-header > .data.percentage").unwrap())
            .next()
            .and_then(|s| {
                let s = s
                    .text()
                    .collect::<String>()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>();

                if s.is_empty() {
                    None
                } else {
                    Some((s.parse::<u32>().unwrap() * u16::MAX as u32 / 100) as u16)
                }
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

fn is_login_screen(document: &Html) -> bool {
    document
        .select(&Selector::parse(".loginscreen").unwrap())
        .next()
        .is_some()
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
        .timeout(Duration::from_secs(3))
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let device_info = get_device_info(client.clone(), addr.clone());
    let supplies_info = get_supplies_info(client.clone(), addr.clone());

    let info = PrinterInfo {
        addr: addr.clone(),
        info: device_info.await?,
        supplies: supplies_info.await?,
    };

    if info.all_null() {
        Err(format!(
            "Could not get any info about supposed printer at address {}",
            addr
        ))
    } else {
        Ok(info)
    }
}

async fn prod_addr_for_printer(own_tx: mpsc::Sender<String>, target: IpAddr) -> bool {
    let data = [1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    let timeout = Duration::from_secs(1);
    //debug!("Pinging address {}..", target);

    if let Ok(_) = ping_rs::send_ping_async(&target, timeout, Arc::new(&data), None).await {
        let saddr = target.to_string();
        let href = href_hp(&saddr, "DeviceStatus/Index");

        debug!(
            "Prodding address {} to check if is compatible printer..",
            target
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

        let res = client.get(href).send().await;
        if let Ok(doc) = res {
            let text = match doc.text().await {
                Ok(text) => text,
                Err(_err) => {
                    return false;
                }
            };

            let html = Html::parse_document(&text);

            if html
                .select(&Selector::parse("body#PageDeviceStatus").unwrap())
                .next()
                .is_some()
            {
                info!("Found printer: {}", target);
                if let Err(err_addr) = own_tx.send(saddr).await {
                    error!("Async receiver dropped before printer address could be sent from contact thread: {}", err_addr);
                    return false;
                }
                return true;
            } else {
                debug!("Is not printer: {}", target);
            }
        };
    }
    false
}

async fn broadcast_ping_printers(tx: mpsc::Sender<String>) -> u32 {
    let mut tried: HashSet<IpAddr> = HashSet::new();

    let ifaces = get_if_addrs();
    let (tc_tx, mut tc_rx) = mpsc::unbounded_channel();
    let task_stream = stream::poll_fn(|c| tc_rx.poll_recv(c));

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

            let network = match ifaddr.clone() {
                V4(addr) => IpNetwork::with_netmask(ip, IpAddr::V4(addr.netmask)),
                V6(addr) => IpNetwork::with_netmask(ip, IpAddr::V6(addr.netmask)),
            };

            let network = match network {
                Ok(network) => network,
                Err(err) => {
                    warn!(
                        "Could not use network info on interface {}: {:?}",
                        iface.name, err
                    );
                    continue;
                }
            };

            let own_tx = tx.clone();
            network
                .iter()
                .filter_map(|target| {
                    if target == ip {
                        None
                    } else {
                        Some((target, prod_addr_for_printer(own_tx.clone(), target)))
                    }
                })
                .for_each(|(target, task)| {
                    if let Err(e) = tc_tx.clone().send(task) {
                        error!("Error sending task to ping {}: {}", target, e);
                    }
                });

            // Check for neighbouring network reachability
            if let V4(addr) = ifaddr {
                let ip = addr.ip;
                let prefix = network.prefix();
                // real  255 255 255 0
                // outer 255 255 0   0
                // rim   0   0   255 0
                let rim_mask = Ipv4Addr::from_bits(0xFFu32 << (32 - prefix));
                let keep_mask = !rim_mask;

                join_all((0u32..=255u32).map(|num| {
                    let tx = tx.clone();
                    let tc_tx = tc_tx.clone();
                    async move {
                        let neigh_ip = Ipv4Addr::from_bits(
                            ip.to_bits() & keep_mask.to_bits()
                                | ((num << (32 - prefix)) & rim_mask.to_bits()),
                        );
                        let neighbor = Ipv4Network::new(neigh_ip, network.prefix()).unwrap();

                        if IpNetwork::V4(neighbor) == network {
                            return;
                        }

                        //debug!("Testing neighbouring subnet: {:?}", neighbor);

                        let own_tx = tx.clone();
                        let gateway = Ipv4Addr::from_bits(
                            neighbor.ip().to_bits() & neighbor.mask().to_bits() | 1,
                        );
                        let data = [1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 0];
                        let timeout = Duration::from_secs(1);

                        if let Ok(_reply) = ping_rs::send_ping_async(
                            &IpAddr::V4(gateway),
                            timeout,
                            Arc::new(&data),
                            None,
                        )
                        .await
                        {
                            debug!("Reached network gateway of neighbor subnet: {:?}", neighbor);

                            neighbor.iter().for_each(|target| {
                                if let Err(err) = tc_tx
                                    .send(prod_addr_for_printer(own_tx.clone(), IpAddr::V4(target)))
                                {
                                    error!(
                                        "Error sending task to check neighboring target {}: {}",
                                        target, err
                                    );
                                }
                            })
                        }
                    }
                }))
                .await;
            }
        }
    }

    drop(tc_tx);
    task_stream
        .buffer_unordered(256)
        .collect::<Vec<_>>()
        .await
        .iter()
        .map(|b| *b as u32)
        .sum()
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

    let num_found = broadcast_ping_printers(tx.clone()).await;
    info!(
        "Automatically found {} printers to process in the network",
        num_found
    );
    get_ips_from_file(tx.clone()).await;
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
