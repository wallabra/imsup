//! Code related to contacting a printer address.

use std::{net::IpAddr, sync::Arc, time::Duration};

use log::{debug, error, info};
use scraper::{Html, Selector};
use tokio::sync::mpsc;

use crate::{
    printerinfo::{DeviceInfo, PrinterInfo, Supplies},
    scrape::{
        get_document_at_href, href_hp, href_hp_device_info, href_hp_supplies_page,
        scrape_device_info, scrape_supplies_info,
    },
};

/// Scrape the [Supplies] from a printer IP address.
async fn get_supplies_info(client: reqwest::Client, addr: String) -> Result<Supplies, String> {
    let href = href_hp_supplies_page(&addr);
    let document = get_document_at_href(&client, href.clone()).await?;
    info!("Got supplies info page for {}: {}", addr, href);
    Ok(scrape_supplies_info(document))
}

/// Scrape the [DeviceInfo] from a printer IP address.
async fn get_device_info(client: reqwest::Client, addr: String) -> Result<DeviceInfo, String> {
    let href = href_hp_device_info(&addr);
    let document = get_document_at_href(&client, href.clone()).await?;
    info!("Got device info page for {}: {}", addr, href);
    Ok(scrape_device_info(document))
}

/// Contact a printer via HTTP and scrapes all possible info from it if
/// applicable.
pub async fn process_addr<'a>(addr: String) -> Result<PrinterInfo, String> {
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

/// Checks if there is a printer at an address.
///
/// If so, its address will be sent through tx.
pub async fn prod_addr_for_printer(tx: mpsc::Sender<String>, target: IpAddr) -> bool {
    let data = [1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    let timeout = Duration::from_secs(1);
    //debug!("Pinging address {}..", target);

    if ping_rs::send_ping_async(&target, timeout, Arc::new(&data), None)
        .await
        .is_ok()
    {
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
                if let Err(err_addr) = tx.send(saddr).await {
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
