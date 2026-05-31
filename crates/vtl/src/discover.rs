use crate::error::{Result, VtlError};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ScsiDeviceKind {
    TapeDrive,
    MediumChanger,
    Disk,
    CdDvd,
    Other,
}

impl ScsiDeviceKind {
    fn from_lsscsi(value: &str) -> Self {
        match value {
            "tape" => Self::TapeDrive,
            "mediumx" => Self::MediumChanger,
            "disk" => Self::Disk,
            "cd/dvd" => Self::CdDvd,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ScsiDevice {
    pub hctl: String,
    pub kind: ScsiDeviceKind,
    pub vendor: String,
    pub product: String,
    pub revision: String,
    pub primary_device: Option<String>,
    pub sg_device: Option<String>,
    pub non_rewinding_device: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct ScsiInventory {
    devices: Vec<ScsiDevice>,
}

impl ScsiInventory {
    pub fn new(devices: Vec<ScsiDevice>) -> Self {
        Self { devices }
    }

    pub fn devices(&self) -> &[ScsiDevice] {
        &self.devices
    }

    pub fn tape_drives(&self) -> Vec<&ScsiDevice> {
        self.devices
            .iter()
            .filter(|device| device.kind == ScsiDeviceKind::TapeDrive)
            .collect()
    }

    pub fn medium_changers(&self) -> Vec<&ScsiDevice> {
        self.devices
            .iter()
            .filter(|device| device.kind == ScsiDeviceKind::MediumChanger)
            .collect()
    }
}

pub fn parse_lsscsi(output: &str) -> Result<ScsiInventory> {
    let mut devices = Vec::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        devices.push(parse_lsscsi_line(line)?);
    }
    Ok(ScsiInventory::new(devices))
}

fn parse_lsscsi_line(line: &str) -> Result<ScsiDevice> {
    let Some(close) = line.find(']') else {
        return Err(VtlError::InvalidLsscsiLine(line.into()));
    };
    let hctl = line
        .strip_prefix('[')
        .and_then(|rest| rest.get(..close - 1))
        .ok_or_else(|| VtlError::InvalidLsscsiLine(line.into()))?
        .to_string();

    let tokens: Vec<&str> = line[close + 1..].split_whitespace().collect();
    if tokens.len() < 4 {
        return Err(VtlError::InvalidLsscsiLine(line.into()));
    }

    let kind = ScsiDeviceKind::from_lsscsi(tokens[0]);
    let vendor = tokens[1].to_string();
    let product = tokens[2].to_string();
    let revision = tokens[3].to_string();
    let primary_device = tokens
        .get(4)
        .filter(|value| value.starts_with("/dev/"))
        .map(|value| (*value).to_string());
    let sg_device = tokens
        .get(5)
        .filter(|value| value.starts_with("/dev/sg"))
        .map(|value| (*value).to_string())
        .or_else(|| {
            tokens
                .iter()
                .find(|value| value.starts_with("/dev/sg"))
                .map(|value| (*value).to_string())
        });
    let non_rewinding_device = primary_device
        .as_deref()
        .and_then(non_rewinding_tape_device);

    Ok(ScsiDevice {
        hctl,
        kind,
        vendor,
        product,
        revision,
        primary_device,
        sg_device,
        non_rewinding_device,
    })
}

fn non_rewinding_tape_device(path: &str) -> Option<String> {
    let suffix = path.strip_prefix("/dev/st")?;
    if suffix.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("/dev/nst{suffix}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_lsscsi_lines() {
        let err = parse_lsscsi_line("not a device").expect_err("invalid line");
        assert!(matches!(err, VtlError::InvalidLsscsiLine(_)));
    }
}
