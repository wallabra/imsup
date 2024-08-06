//! Network scan utilities to find printers.

use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};

use futures::{future::join_all, stream, StreamExt};
use get_if_addrs::{get_if_addrs, IfAddr};
use ipnetwork::{IpNetwork, Ipv4Network};
use log::{debug, error, info, warn};
use tokio::sync::mpsc;

use crate::contact::prod_addr_for_printer;

/// Scans the network for printers.
///
/// All printer addresses found will be sent through tx.
///
/// If [cluster_search] is true and the network is at most /24 big, neighboring
/// networks will also be scanned for printers.
pub async fn broadcast_ping_printers(
    tx: mpsc::Sender<String>,
    cluster_search: bool,
    ping_parallel: usize,
) -> u32 {
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
                IfAddr::V4(addr) => IpNetwork::with_netmask(ip, IpAddr::V4(addr.netmask)),
                IfAddr::V6(addr) => IpNetwork::with_netmask(ip, IpAddr::V6(addr.netmask)),
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
            if cluster_search {
                let prefix = network.prefix();
                if let IfAddr::V4(addr) = ifaddr {
                    if prefix < 24 {
                        continue;
                    }

                    let ip = addr.ip;
                    // real  255 255 255 0
                    // outer 255 255 0   0
                    // rim   0   0   255 0
                    let rim_mask = Ipv4Addr::from_bits(0xFFu32 << (32 - prefix));
                    let keep_mask = !rim_mask;

                    join_all((0u32..=254u32).map(|num| {
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
                                debug!(
                                    "Reached network gateway of neighbor subnet: {:?}",
                                    neighbor
                                );

                                neighbor.iter().for_each(|target| {
                                    if let Err(err) = tc_tx.send(prod_addr_for_printer(
                                        own_tx.clone(),
                                        IpAddr::V4(target),
                                    )) {
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
    }

    drop(tc_tx);
    task_stream
        .buffer_unordered(ping_parallel)
        .collect::<Vec<_>>()
        .await
        .iter()
        .map(|b| *b as u32)
        .sum()
}
