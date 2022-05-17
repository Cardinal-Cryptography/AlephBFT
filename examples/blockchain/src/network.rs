use crate::{blockchain::Transaction, consensus::ConsensusData, io::NetworkConsensusIO};
use aleph_bft::Recipient;
use codec::{Decode, Encode};
use futures::{FutureExt, StreamExt};
use futures_timer::Delay;
use log::error;
use std::{
    collections::HashMap,
    io::Write,
    net::{SocketAddr, TcpStream},
    str::FromStr,
};
use tokio::{
    io::{self, AsyncReadExt},
    net::TcpListener,
};

#[derive(Clone, Debug, Decode, Encode)]
pub struct Address {
    octets: [u8; 4],
    port: u16,
}

impl FromStr for Address {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let addr = s.parse::<SocketAddr>().unwrap();
        let port = addr.port();
        assert_ne!(port, 0);
        let octets = match addr {
            SocketAddr::V4(addr) => addr.ip().octets(),
            _ => panic!(),
        };
        Ok(Self { octets, port })
    }
}

impl Address {
    pub async fn new_bind(ip_addr: String) -> (TcpListener, Self) {
        let listener = TcpListener::bind(ip_addr.parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        let ip_addr = listener.local_addr().unwrap().to_string();
        (listener, Self::from_str(&ip_addr).unwrap())
    }
    pub fn connect(&self) -> io::Result<TcpStream> {
        TcpStream::connect(SocketAddr::from((self.octets, self.port)))
    }
}

#[derive(Clone, Debug, Decode, Encode)]
pub enum Message {
    DNSNodeRequest(u32, Address),
    DNSClientRequest(Address),
    DNSResponse(Vec<Option<Address>>),
    Transaction(Transaction),
    Consensus(Box<ConsensusData>),
}

pub struct Network {
    id: usize,
    address: Address,
    addresses: Vec<Option<Address>>,
    listener: TcpListener,
    consensus_io: NetworkConsensusIO,
}

impl Network {
    pub async fn new(
        id: usize,
        ip_addr: String,
        n_nodes: usize,
        bootnodes: HashMap<u32, Address>,
        consensus_io: NetworkConsensusIO,
    ) -> Self {
        let mut addresses = vec![None; n_nodes];
        for (id, addr) in bootnodes {
            addresses[id as usize] = Some(addr);
        }
        let (listener, address) = Address::new_bind(ip_addr).await;
        addresses[id] = Some(address.clone());
        Self {
            id,
            address,
            addresses,
            listener,
            consensus_io,
        }
    }

    fn send(&self, message: Message, recipient: Recipient) {
        match recipient {
            Recipient::Node(n) => self.try_send_to_peer(message, n.into()).unwrap_or(()),
            Recipient::Everyone => {
                for n in 0..self.addresses.len() {
                    if n != self.id {
                        self.try_send_to_peer(message.clone(), n).unwrap_or(());
                    };
                }
            }
        };
    }

    fn try_send_to_peer(&self, message: Message, recipient: usize) -> io::Result<()> {
        assert!(recipient < self.addresses.len());
        if let Some(addr) = &self.addresses[recipient] {
            self.try_send(message, addr)?;
        }
        Ok(())
    }

    fn try_send(&self, message: Message, address: &Address) -> io::Result<()> {
        address.connect()?.write_all(&message.encode())
    }

    fn dns_node_request(&mut self, id: usize, address: Address) {
        self.addresses[id] = Some(address.clone());
        self.dns_request(address);
    }

    fn dns_request(&self, address: Address) {
        self.try_send(Message::DNSResponse(self.addresses.clone()), &address)
            .unwrap_or(())
    }

    pub async fn run(&mut self) {
        let ticker_delay = std::time::Duration::from_millis(3000);
        let mut ticker = Delay::new(ticker_delay).fuse();
        let dns_ticker_delay = std::time::Duration::from_millis(1000);
        let mut dns_ticker = Delay::new(dns_ticker_delay).fuse();

        loop {
            let mut buffer = Vec::new();
            tokio::select! {

                event = self.consensus_io.rx_data.next() => if let Some((data, recipient)) = event {
                    self.send(Message::Consensus(Box::new(data)), recipient);
                },

                event = self.listener.accept() => match event {
                    Ok((mut socket, _addr)) => match socket.read_to_end(&mut buffer).await {
                        Ok(_) => match Message::decode(&mut &buffer[..]) {
                            Ok(Message::Transaction(tr)) => { self.consensus_io.tx_transactions.unbounded_send(tr).unwrap();},
                            Ok(Message::Consensus(data)) => { self.consensus_io.tx_data.unbounded_send(*data).unwrap();},
                            Ok(Message::DNSNodeRequest(id, address)) => self.dns_node_request(id as usize, address),
                            Ok(Message::DNSClientRequest(address)) => self.dns_request(address),
                            Ok(Message::DNSResponse(addresses)) => for (id, addr) in addresses.iter().enumerate() {
                                if let Some(addr) = addr {
                                    self.addresses[id as usize] = Some(addr.clone());
                                };
                            },
                            Err(_) => (),
                        },
                        Err(_) => {
                            error!("Could not decode incoming data");
                        }
                    },
                    Err(e) => {
                        error!("Couldn't accept connection: {:?}", e);
                    },
                },

                _ = &mut ticker => {
                    self.send(Message::Transaction(Transaction::Print(0,10)), Recipient::Everyone);
                    self.send(Message::Transaction(Transaction::Burn(0,20)), Recipient::Everyone);
                    ticker = Delay::new(ticker_delay).fuse();
                },

                _ = &mut dns_ticker => {
                    self.send(Message::DNSNodeRequest(self.id as u32, self.address.clone()), Recipient::Everyone);
                    if self.addresses.iter().any(|a| a.is_none()) {
                        dns_ticker = Delay::new(dns_ticker_delay).fuse();
                    }
                },
            }
        }
    }
}
