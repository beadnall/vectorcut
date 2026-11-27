//! Silhouette machines on USB, through `nusb` (pure Rust: IOKit on macOS, usbfs on Linux, WinUSB
//! on Windows).
//!
//! The machines are USB printer-class devices with one bulk OUT endpoint for commands and one
//! bulk IN endpoint for replies (0x01 and 0x82 on the machines the community drivers describe).
//! The endpoints are read from the interface descriptor, with those addresses as the fallback.
//!
//! Platform notes:
//! - macOS: no driver holds the interface; it opens as it is. Quit Silhouette Studio first, which
//!   keeps the machine open while it runs.
//! - Linux: the `usblp` printer driver binds the interface; it is detached when the interface is
//!   claimed. The user needs write access to the device (a udev rule).
//! - Windows: the machine is bound to the system printer driver, which `nusb` cannot open; the
//!   interface has to be switched to WinUSB (Zadig) until a native backend exists.

use crate::link::{Link, LinkError};
use nusb::MaybeFuture as _;
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
use nusb::{DeviceInfo, Endpoint, Interface};
use std::time::Duration;
use vectorcut_core::Model;

/// Default endpoint addresses (bulk OUT, bulk IN).
const OUT_ENDPOINT: u8 = 0x01;
const IN_ENDPOINT: u8 = 0x82;

/// Why a machine could not be found or opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsbError {
    #[error("could not list USB devices: {0}")]
    List(String),
    #[error("could not open the machine: {message}{hint}")]
    Open { message: String, hint: &'static str },
}

fn open_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        " — if Silhouette Studio is running, quit it and try again"
    } else if cfg!(target_os = "linux") {
        " — check that your user may write to the device (a udev rule for vendor 0b4d)"
    } else if cfg!(target_os = "windows") {
        " — the machine needs the WinUSB driver (install it for the device with Zadig)"
    } else {
        ""
    }
}

fn open_error(error: impl std::fmt::Display) -> UsbError {
    UsbError::Open { message: error.to_string(), hint: open_hint() }
}

/// A Silhouette machine found on USB.
#[derive(Debug, Clone)]
pub struct Found {
    pub vendor_id: u16,
    pub product_id: u16,
    /// The model the USB ids name, when VectorCut knows them.
    pub model: Option<&'static Model>,
    /// The product string the machine reports.
    pub product: Option<String>,
    pub serial: Option<String>,
    info: DeviceInfo,
}

impl Found {
    /// A name for people: the model, else the product string, else the ids.
    pub fn label(&self) -> String {
        match (self.model, &self.product) {
            (Some(model), _) => format!("Silhouette {}", model.name),
            (None, Some(product)) => product.clone(),
            (None, None) => format!("Unknown Silhouette ({:04x}:{:04x})", self.vendor_id, self.product_id),
        }
    }

    /// A key that stays the same while the machine stays plugged in.
    pub fn key(&self) -> String {
        format!("{:04x}:{:04x}:{:?}", self.vendor_id, self.product_id, self.info.id())
    }

    /// Opens the machine and claims its interface.
    pub fn open(&self) -> Result<UsbLink, UsbError> {
        let device = self.info.open().wait().map_err(open_error)?;
        let number = self.info.interfaces().next().map_or(0, nusb::InterfaceInfo::interface_number);
        let interface = device.detach_and_claim_interface(number).wait().map_err(open_error)?;
        let (mut out_addr, mut in_addr) = (OUT_ENDPOINT, IN_ENDPOINT);
        if let Some(descriptor) = interface.descriptor() {
            for ep in descriptor.endpoints().filter(|ep| ep.transfer_type() == TransferType::Bulk) {
                if ep.address() & 0x80 == 0 {
                    out_addr = ep.address();
                } else {
                    in_addr = ep.address();
                }
            }
        }
        let out = interface.endpoint::<Bulk, Out>(out_addr).map_err(open_error)?;
        let input = interface.endpoint::<Bulk, In>(in_addr).map_err(open_error)?;
        Ok(UsbLink { out, input, _interface: interface, label: self.label() })
    }
}

/// The Silhouette machines plugged in now.
pub fn find() -> Result<Vec<Found>, UsbError> {
    let devices = nusb::list_devices().wait().map_err(|e| UsbError::List(e.to_string()))?;
    Ok(devices
        .filter(|d| Model::is_silhouette_vendor(d.vendor_id()))
        .map(|d| Found {
            vendor_id: d.vendor_id(),
            product_id: d.product_id(),
            model: Model::by_usb(d.vendor_id(), d.product_id()),
            product: d.product_string().map(str::to_string),
            serial: d.serial_number().map(str::to_string),
            info: d,
        })
        .collect())
}

/// An open machine on USB.
pub struct UsbLink {
    out: Endpoint<Bulk, Out>,
    input: Endpoint<Bulk, In>,
    /// Keeps the claim on the interface while the endpoints are in use.
    _interface: Interface,
    label: String,
}

fn transfer_error(error: TransferError) -> LinkError {
    match error {
        TransferError::Disconnected => LinkError::Disconnected,
        TransferError::Stall => LinkError::Stalled,
        other => LinkError::Io(format!("{other:?}")),
    }
}

impl Link for UsbLink {
    fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, LinkError> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let done = self.out.transfer_blocking(Buffer::from(bytes.to_vec()), timeout);
        match done.status {
            Ok(()) => Ok(done.actual_len),
            // A timeout cancels the transfer; whatever went out before it counts.
            Err(TransferError::Cancelled) => Ok(done.actual_len),
            Err(TransferError::Stall) => {
                let _ = self.out.clear_halt().wait();
                Err(LinkError::Stalled)
            }
            Err(other) => Err(transfer_error(other)),
        }
    }

    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        // An IN transfer asks for a whole number of packets.
        let packet = self.input.max_packet_size().max(1);
        let done = self.input.transfer_blocking(Buffer::new(packet), timeout);
        match done.status {
            Ok(()) => Ok(done.buffer.get(..done.actual_len).unwrap_or_default().to_vec()),
            Err(TransferError::Cancelled) => Ok(done.buffer.get(..done.actual_len).unwrap_or_default().to_vec()),
            Err(TransferError::Stall) => {
                let _ = self.input.clear_halt().wait();
                Err(LinkError::Stalled)
            }
            Err(other) => Err(transfer_error(other)),
        }
    }

    fn describe(&self) -> String {
        format!("{} (USB)", self.label)
    }
}
