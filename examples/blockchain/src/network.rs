use crate::{blockchain::Transaction, consensus::ConsensusData, io::NetworkConsensusIO};
use aleph_bft::Recipient;
use codec::{Decode, Encode};
use futures::{
    channel::{mpsc::UnboundedReceiver, oneshot},
    FutureExt, StreamExt,
};
use futures_timer::Delay;
use log::{error, info};
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
    Crash,
}

pub struct NodeNetwork {
    id: usize,
    address: Address,
    addresses: Vec<Option<Address>>,
    bootnodes: HashMap<u32, Address>,
    listener: TcpListener,
    consensus_io: NetworkConsensusIO,
    crash: Option<oneshot::Sender<()>>,
}

impl NodeNetwork {
    pub async fn new(
        id: usize,
        ip_addr: String,
        n_nodes: usize,
        bootnodes: HashMap<u32, Address>,
        consensus_io: NetworkConsensusIO,
        crash: oneshot::Sender<()>,
    ) -> Self {
        let mut addresses = vec![None; n_nodes];
        for (id, addr) in &bootnodes {
            addresses[*id as usize] = Some(addr.clone());
        }
        let (listener, address) = Address::new_bind(ip_addr).await;
        addresses[id] = Some(address.clone());
        Self {
            id,
            address,
            addresses,
            bootnodes,
            listener,
            consensus_io,
            crash: Some(crash),
        }
    }

    fn reset_dns(&mut self, n: usize) {
        if !self.bootnodes.contains_key(&(n as u32)) {
            error!("Reseting address of node {}", n);
            self.addresses[n] = None;
        }
    }

    fn send(&mut self, message: Message, recipient: Recipient) {
        match recipient {
            Recipient::Node(n) => match self.try_send_to_peer(message, n.into()) {
                Ok(_) => (),
                Err(_) => self.reset_dns(n.into()),
            },
            Recipient::Everyone => {
                for n in 0..self.addresses.len() {
                    if n != self.id {
                        match self.try_send_to_peer(message.clone(), n) {
                            Ok(_) => (),
                            Err(_) => self.reset_dns(n),
                        }
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
                            Ok(Message::Transaction(tr)) => self.consensus_io.tx_transactions.unbounded_send(tr).unwrap_or(()),
                            Ok(Message::Consensus(data)) => self.consensus_io.tx_data.unbounded_send(*data).unwrap_or(()),
                            Ok(Message::DNSNodeRequest(id, address)) => self.dns_node_request(id as usize, address),
                            Ok(Message::DNSClientRequest(address)) => self.dns_request(address),
                            Ok(Message::DNSResponse(addresses)) => for (id, addr) in addresses.iter().enumerate() {
                                if let Some(addr) = addr {
                                    self.addresses[id as usize] = Some(addr.clone());
                                };
                            },
                            Ok(Message::Crash) => if let Some(ch) = self.crash.take() {
                                ch.send(()).unwrap_or(());
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

                _ = &mut dns_ticker => {
                    if self.addresses.iter().any(|a| a.is_none()) {
                        self.send(Message::DNSNodeRequest(self.id as u32, self.address.clone()), Recipient::Everyone);
                        info!("Requesting IP addresses");
                    }
                    dns_ticker = Delay::new(dns_ticker_delay).fuse();
                },
            }
        }
    }
}

pub struct ClientNetwork {
    address: Address,
    addresses: Vec<Option<Address>>,
    bootnodes: HashMap<u32, Address>,
    listener: TcpListener,
    rx_command: UnboundedReceiver<String>,
}

impl ClientNetwork {
    pub async fn new(
        ip_addr: String,
        n_nodes: usize,
        bootnodes: HashMap<u32, Address>,
        rx_command: UnboundedReceiver<String>,
    ) -> Self {
        let mut addresses = vec![None; n_nodes];
        for (id, addr) in &bootnodes {
            addresses[*id as usize] = Some(addr.clone());
        }
        let (listener, address) = Address::new_bind(ip_addr).await;
        Self {
            address,
            addresses,
            bootnodes,
            listener,
            rx_command,
        }
    }

    fn reset_dns(&mut self, n: usize) {
        if !self.bootnodes.contains_key(&(n as u32)) {
            error!("Reseting address of node {}", n);
            self.addresses[n] = None;
        }
    }

    fn send(&mut self, message: Message, recipient: usize) {
        if recipient >= self.addresses.len() {
            for recipient in 0..self.addresses.len() {
                self.send(message.clone(), recipient);
            }
        } else {
            match self.try_send_to_node(message, recipient) {
                Ok(_) => (),
                Err(_) => self.reset_dns(recipient),
            }
        }
    }

    fn try_send_to_node(&self, message: Message, recipient: usize) -> io::Result<()> {
        assert!(recipient < self.addresses.len());
        if let Some(addr) = &self.addresses[recipient] {
            addr.connect()?.write_all(&message.encode())?;
            println!("Message {:?} sent to {:?}.", message, addr);
        }
        Ok(())
    }

    fn command(&mut self, command: String) {
        let mut command = command
            .split_whitespace()
            .map(|s| s.to_string())
            .collect::<Vec<String>>();
        let mut n_times = 1;
        if command.is_empty() {
            println!("Command empty.");
            return;
        }
        if let Ok(n) = command[0].parse::<u32>() {
            n_times = n;
            command = command[1..].to_vec();
        };
        if command.len() < 2 {
            println!("Missing recipient.");
            return;
        }
        let (command, args, recipient) = (
            command[0].clone(),
            command[1..command.len() - 1].to_vec(),
            command[command.len() - 1].clone(),
        );
        let recipient_id: usize;
        if recipient == *"broadcast".to_string() {
            recipient_id = self.addresses.len();
        } else {
            recipient_id = recipient.parse::<usize>().unwrap_or(0);
        }
        let args: Vec<u32> = args
            .into_iter()
            .map(|s| s.parse::<u32>().unwrap_or(0))
            .collect();
        let message = match (command.as_str(), args.len()) {
            ("crash", 0) => Message::Crash,
            ("print", 2) => Message::Transaction(Transaction::Print(args[0], args[1])),
            ("burn", 2) => Message::Transaction(Transaction::Burn(args[0], args[1])),
            ("transfer", 3) => {
                Message::Transaction(Transaction::Transfer(args[0], args[1], args[2]))
            }
            _ => {
                println!("Command not recognized:");
                println!("{} {:?}", command.as_str(), args);
                return;
            }
        };
        for _ in 0..n_times {
            println!("Trying to send {:?} to {}", message, recipient_id);
            self.send(message.clone(), recipient_id);
        }
    }

    pub async fn run(&mut self) {
        let dns_ticker_delay = std::time::Duration::from_millis(1000);
        let mut dns_ticker = Delay::new(dns_ticker_delay).fuse();

        loop {
            let mut buffer = Vec::new();
            tokio::select! {
                event = self.listener.accept() => match event {
                    Ok((mut socket, _addr)) => match socket.read_to_end(&mut buffer).await {
                        Ok(_) => match Message::decode(&mut &buffer[..]) {
                            Ok(Message::DNSResponse(addresses)) => for (id, addr) in addresses.iter().enumerate() {
                                if let Some(addr) = addr {
                                    self.addresses[id as usize] = Some(addr.clone());
                                };
                            },
                            Ok(_) => (),
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

                command = self.rx_command.next() => self.command(command.unwrap()),

                _ = &mut dns_ticker => {
                    if self.addresses.iter().any(|a| a.is_none()) {
                        self.send(Message::DNSClientRequest(self.address.clone()), 5);
                    }
                    dns_ticker = Delay::new(dns_ticker_delay).fuse();
                },
            }
        }
    }
}
