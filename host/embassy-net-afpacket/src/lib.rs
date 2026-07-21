// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! `embassy-net` driver that uses a Linux `AF_PACKET`/`SOCK_RAW` socket bound
//! to an existing network interface. It lets a host-side embassy-net stack
//! send and receive raw Ethernet frames on a real NIC, the way the firmware
//! talks to its on-chip MAC.
//!
//! The shape mirrors `embassy-net-tuntap`: open an fd, query MTU, wrap in
//! `async_io::Async`, and implement `embassy_net_driver::Driver`. The
//! difference is that the link-layer is provided by the host kernel
//! instead of a synthetic TAP. Two modes are exposed: `new` reports the
//! interface's real MAC (read via `SIOCGIFHWADDR`); `with_synthetic_mac`
//! takes a caller-supplied MAC and puts the NIC in promiscuous mode so
//! frames addressed to that synthetic MAC actually reach the driver.
//!
//! Caveat: the kernel keeps processing ingress frames on the same
//! interface in parallel with this driver. Point it at a NIC where the
//! kernel has no IPv4 configured (e.g. `ip addr flush dev …`) or use a
//! veth pair if you need to coexist with the host stack on the same wire.

#![warn(missing_docs)]
// FFI glue: every `as` cast below is intentional and bounded by the
// kernel ABI. Listing the lints once at module scope keeps the call
// sites readable.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::io::{AsRawFd, RawFd};
use std::task::Context;

use async_io::Async;
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState};
use log::info;

/// Length of an Ethernet header. smoltcp counts L2 in its MTU, so this gets
/// added to the IP MTU returned by `SIOCGIFMTU` before being reported as
/// `Capabilities::max_transmission_unit`.
const ETHERNET_HEADER_LEN: usize = 14;

/// Generic interface request structure used by the SIOC* ioctls. The C union
/// in `<net/if.h>` has multiple members; here it is represented as a
/// 24-byte buffer that is large enough to hold any of the variants we use
/// (`ifr_ifindex`, `ifr_mtu`, `ifr_hwaddr`).
#[repr(C)]
struct Ifreq {
    name: [libc::c_char; libc::IF_NAMESIZE],
    data: [u8; 24],
}

impl Ifreq {
    fn new(name: &str) -> io::Result<Self> {
        if name.len() >= libc::IF_NAMESIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interface name too long",
            ));
        }
        let mut request = Self {
            name: [0; libc::IF_NAMESIZE],
            data: [0; 24],
        };
        for (slot, byte) in request.name.iter_mut().zip(name.as_bytes()) {
            *slot = *byte as libc::c_char;
        }
        Ok(request)
    }
}

fn ioctl(fd: libc::c_int, command: libc::c_ulong, request: &mut Ifreq) -> io::Result<()> {
    let result = unsafe { libc::ioctl(fd, command as _, std::ptr::from_mut(request)) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Raw `AF_PACKET` socket bound to a single interface. Reads return the
/// next full Ethernet frame seen on the interface; writes inject a frame
/// out of it.
#[derive(Debug)]
pub struct AfPacket {
    fd: libc::c_int,
    mtu: usize,
    hwaddr: [u8; 6],
}

impl AsRawFd for AfPacket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl AfPacket {
    /// Open a non-blocking `AF_PACKET`/`SOCK_RAW` socket and bind it to the
    /// named interface, reporting the interface's real MAC. Requires
    /// `CAP_NET_RAW` (run as root or set the capability on the binary).
    ///
    /// # Errors
    ///
    /// Returns the underlying `io::Error` if the socket cannot be opened,
    /// the interface is unknown, or any of the `SIOC*` ioctls fail.
    pub fn new(name: &str) -> io::Result<Self> {
        Self::open(name, None)
    }

    /// Like [`Self::new`], but reports the supplied `hwaddr` to the embassy
    /// stack instead of the kernel's interface MAC. The NIC is put in
    /// promiscuous mode for the lifetime of the socket so frames addressed
    /// to the synthetic MAC actually reach this driver.
    ///
    /// # Errors
    ///
    /// Returns the underlying `io::Error` if the socket cannot be opened,
    /// the interface is unknown, or any of the `SIOC*` / setsockopt calls
    /// fail.
    pub fn with_synthetic_mac(name: &str, hwaddr: [u8; 6]) -> io::Result<Self> {
        Self::open(name, Some(hwaddr))
    }

    fn open(name: &str, override_hwaddr: Option<[u8; 6]>) -> io::Result<Self> {
        // ETH_P_ALL is in host byte order in the libc constant; the kernel
        // expects it big-endian on the wire-facing fields.
        let protocol = (libc::ETH_P_ALL as u16).to_be();
        let socket_type = libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC;
        let fd = unsafe { libc::socket(libc::AF_PACKET, socket_type, libc::c_int::from(protocol)) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }

        let result = Self::configure(fd, name, override_hwaddr);
        match result {
            Ok((mtu, hwaddr)) => Ok(Self { fd, mtu, hwaddr }),
            Err(error) => {
                unsafe { libc::close(fd) };
                Err(error)
            }
        }
    }

    fn configure(
        fd: libc::c_int,
        name: &str,
        override_hwaddr: Option<[u8; 6]>,
    ) -> io::Result<(usize, [u8; 6])> {
        let mut index_request = Ifreq::new(name)?;
        ioctl(fd, libc::SIOCGIFINDEX, &mut index_request)?;
        // SAFETY: SIOCGIFINDEX writes an `int` into the first 4 bytes of
        // the union, which is exactly what we read back here.
        let ifindex = i32::from_ne_bytes(
            index_request.data[..4]
                .try_into()
                .expect("BUG: 4-byte slice fits in [u8; 4]"),
        );

        let mut mtu_request = Ifreq::new(name)?;
        ioctl(fd, libc::SIOCGIFMTU, &mut mtu_request)?;
        let ip_mtu = i32::from_ne_bytes(
            mtu_request.data[..4]
                .try_into()
                .expect("BUG: 4-byte slice fits in [u8; 4]"),
        );
        if ip_mtu <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "interface reported non-positive MTU",
            ));
        }
        let mtu = (ip_mtu as usize) + ETHERNET_HEADER_LEN;

        let hwaddr = if let Some(addr) = override_hwaddr {
            addr
        } else {
            let mut hw_request = Ifreq::new(name)?;
            ioctl(fd, libc::SIOCGIFHWADDR, &mut hw_request)?;
            // SIOCGIFHWADDR fills the union with `struct sockaddr`.
            // Layout is `u16 sa_family` followed by 14 bytes of
            // `sa_data`; the MAC sits in the first 6 bytes of `sa_data`.
            let mut addr = [0u8; 6];
            addr.copy_from_slice(&hw_request.data[2..8]);
            addr
        };

        let bind_addr = libc::sockaddr_ll {
            sll_family: libc::AF_PACKET as libc::c_ushort,
            sll_protocol: (libc::ETH_P_ALL as u16).to_be(),
            sll_ifindex: ifindex,
            sll_hatype: 0,
            sll_pkttype: 0,
            sll_halen: 0,
            sll_addr: [0; 8],
        };
        let result = unsafe {
            libc::bind(
                fd,
                std::ptr::addr_of!(bind_addr).cast::<libc::sockaddr>(),
                mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }

        // A synthetic MAC needs the NIC in promiscuous mode; otherwise
        // hardware filtering drops frames addressed to it. The kernel
        // reference-counts membership and restores the previous state when
        // the socket is closed.
        if override_hwaddr.is_some() {
            enable_promisc(fd, ifindex)?;
        }

        Ok((mtu, hwaddr))
    }
}

fn enable_promisc(fd: libc::c_int, ifindex: libc::c_int) -> io::Result<()> {
    let request = libc::packet_mreq {
        mr_ifindex: ifindex,
        mr_type: libc::PACKET_MR_PROMISC as libc::c_ushort,
        mr_alen: 0,
        mr_address: [0; 8],
    };
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_PACKET,
            libc::PACKET_ADD_MEMBERSHIP,
            std::ptr::addr_of!(request).cast::<libc::c_void>(),
            mem::size_of::<libc::packet_mreq>() as libc::socklen_t,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Drop for AfPacket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

impl Read for AfPacket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = unsafe {
            libc::read(
                self.fd,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if length == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(length as usize)
    }
}

impl Write for AfPacket {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = unsafe {
            libc::write(
                self.fd,
                buffer.as_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if length == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(length as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Async wrapper around an `AfPacket` socket. Implements
/// `embassy_net_driver::Driver` so it can plug into `embassy_net::new`
/// like any other link-layer driver.
pub struct AfPacketDevice {
    device: Async<AfPacket>,
}

impl AfPacketDevice {
    /// Open the underlying raw socket and prepare it for async use. The
    /// embassy stack will see the interface's real MAC.
    ///
    /// # Errors
    ///
    /// Returns the underlying `io::Error` if the socket cannot be opened,
    /// the interface is unknown, or any of the `SIOC*` ioctls fail.
    pub fn new(name: &str) -> io::Result<Self> {
        Ok(Self {
            device: Async::new(AfPacket::new(name)?)?,
        })
    }

    /// Like [`Self::new`], but uses the supplied locally-administered MAC
    /// instead of the kernel's interface MAC, and enables promiscuous mode
    /// on the NIC so frames addressed to the synthetic MAC are delivered.
    ///
    /// # Errors
    ///
    /// Returns the underlying `io::Error` if the socket cannot be opened,
    /// the interface is unknown, or any of the `SIOC*` / setsockopt calls
    /// fail.
    pub fn with_synthetic_mac(name: &str, hwaddr: [u8; 6]) -> io::Result<Self> {
        Ok(Self {
            device: Async::new(AfPacket::with_synthetic_mac(name, hwaddr)?)?,
        })
    }
}

impl Driver for AfPacketDevice {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, context: &mut Context) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buffer = vec![0; self.device.get_ref().mtu];
        loop {
            match self.device.get_mut().read(&mut buffer) {
                Ok(length) => {
                    buffer.truncate(length);
                    return Some((
                        RxToken { buffer },
                        TxToken {
                            device: &mut self.device,
                        },
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if !self.device.poll_readable(context).is_ready() {
                        return None;
                    }
                }
                Err(error) => panic!("BUG: AF_PACKET read failed: {error:?}"),
            }
        }
    }

    fn transmit(&mut self, _context: &mut Context) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            device: &mut self.device,
        })
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.max_transmission_unit = self.device.get_ref().mtu;
        capabilities
    }

    fn link_state(&mut self, _context: &mut Context) -> LinkState {
        LinkState::Up
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet(self.device.get_ref().hwaddr)
    }
}

/// Receive-side token holding one frame read from the raw socket.
#[doc(hidden)]
pub struct RxToken {
    buffer: Vec<u8>,
}

impl embassy_net_driver::RxToken for RxToken {
    fn consume<R, F>(mut self, callback: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        callback(&mut self.buffer)
    }
}

/// Transmit-side token. Borrows the underlying socket and writes the frame
/// produced by smoltcp once the callback fills the buffer.
#[doc(hidden)]
pub struct TxToken<'a> {
    device: &'a mut Async<AfPacket>,
}

impl embassy_net_driver::TxToken for TxToken<'_> {
    fn consume<R, F>(self, length: usize, callback: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0; length];
        let result = callback(&mut buffer);

        match self.device.get_mut().write(&buffer) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                info!("AF_PACKET transmit WouldBlock, frame dropped");
            }
            Err(error) => panic!("BUG: AF_PACKET write failed: {error:?}"),
        }

        result
    }
}
