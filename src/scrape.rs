use scraper::{ElementRef, Html, Selector};

use crate::printerinfo::{DeviceInfo, Supplies, SuppliesCartridge};

/// Returns a standard HP printer internal webpage URL.
pub fn href_hp(addr: &str, path: &str) -> String {
    format!("http://{}/hp/device/{}", addr, path)
}

/// Returns a standard HP printer supplies status page URL.
pub fn href_hp_supplies_page(addr: &str) -> String {
    href_hp(addr, "InternalPages/Index?id=SuppliesStatus")
}

/// Returns a standard HP printer device information page URL.
pub fn href_hp_device_info(addr: &str) -> String {
    href_hp(addr, "DeviceInformation/View")
}

/// Checks if a contacted page is a login screen.
///
/// This is made to filter out invalid hosts, which either are not printers
/// or are not accessible to the scraper anyway.
pub fn is_login_screen(document: &Html) -> bool {
    document
        .select(&Selector::parse(".loginscreen").unwrap())
        .next()
        .is_some()
}

/// Sends a request to a HTTP address and returns a parsed HTML if successful.
pub async fn get_document_at_href(client: &reqwest::Client, href: String) -> Result<Html, String> {
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

/// Select a single cartridge's div block out of a supplies status page.
fn get_cartridge_block<'a, 'h: 'a>(frag: &'h Html, colname: &str) -> Option<ElementRef<'a>> {
    frag.select(&Selector::parse(&format!(".consumable-block-{}", colname)).unwrap())
        .next()
}

/// Extract a single datum from a cartridge block.
fn get_cartridge_row(block: &ElementRef<'_>, row_name: &str) -> Option<String> {
    block
        .select(&Selector::parse(&format!("[id$=\"{}\"]", row_name)).unwrap())
        .next()
        .map(|r| r.text().collect::<String>())
}

/// Parses all information relevant to a single color cartridge.
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

/// Scrapes information from a supplies status page, once for each color.
pub fn scrape_supplies_info(document: Html) -> Supplies {
    Supplies {
        c: scrape_cartridge_color("cyan", &document),
        m: scrape_cartridge_color("magenta", &document),
        y: scrape_cartridge_color("yellow", &document),
        k: scrape_cartridge_color("black", &document),
    }
}

/// Get a single datum of information from the device info page.
pub fn get_device_info_datum(doc: &Html, infoname: &str) -> Option<String> {
    Some(
        doc.select(&Selector::parse(&format!("#{}", &infoname)).unwrap())
            .next()?
            .text()
            .collect::<String>(),
    )
}

/// Scrapes information from a device info page.
pub fn scrape_device_info(document: Html) -> DeviceInfo {
    DeviceInfo {
        location: get_device_info_datum(&document, "DeviceLocation"),
        model: get_device_info_datum(&document, "DeviceModel"),
        name: get_device_info_datum(&document, "ProductName"),
        nick: get_device_info_datum(&document, "DeviceName"),
        serial: get_device_info_datum(&document, "DeviceSerialNumber"),
    }
}
