use serde_derive::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
/// The information collected from the HP device information page.
///
/// See [href_device_info].
pub struct DeviceInfo {
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
/// A single color cartridge.
///
/// Can be cyan, magenta, yellow or key (black).
///
/// This struct does not tell which color the cartridge is, because you're not
/// supposed to access this struct outside the context of a Supplies struct.
pub struct SuppliesCartridge {
    pub estim_pages_remaining: Option<u32>,
    pub printed: Option<u32>,
    pub ink_level: Option<u16>,
}

impl SuppliesCartridge {
    pub fn all_null(&self) -> bool {
        self.estim_pages_remaining.is_none() && self.printed.is_none() && self.ink_level.is_none()
    }
}

/// The information collected from the HP supplies status page.
///
/// See [href_supplies_page].
#[derive(Serialize, Deserialize)]
pub struct Supplies {
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

/// The information collected about a printer.
#[derive(Serialize, Deserialize)]
pub struct PrinterInfo {
    pub addr: String,
    pub info: DeviceInfo,
    pub supplies: Supplies,
}

impl PrinterInfo {
    /// Whether no information was gathered about this printer at all.
    ///
    /// Such instances usually mean that an issue was had while trying
    /// to read from a printer. They should not happen if the filters are
    /// sufficient. Currently, only login pages are filtered out.
    pub fn all_null(&self) -> bool {
        self.info.all_null() && self.supplies.all_null()
    }
}
