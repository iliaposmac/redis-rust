use core::panic;
use std::{
    io::{Error, ErrorKind, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    os::fd::{AsFd, AsRawFd, BorrowedFd},
};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use socket2::{Domain, Protocol, Socket, Type};

// https://www.reddit.com/r/rust/comments/r04zod/comment/hlqhpoc/?utm_source=share&utm_medium=web2x&context=3

fn main() {
    #[cfg(feature = "redis_server")]
    redis_server();

    #[cfg(feature = "redis_client")]
    redis_client();
}

#[derive(Default)]
pub struct SocketConnection {
    pub stream: Option<TcpStream>,
    pub want_read: bool,
    pub want_write: bool,
    pub want_close: bool,

    pub incoming_data: Vec<u8>,
    pub outgoing_data: Vec<u8>,
}

impl SocketConnection {
    pub fn get_fd(&self) -> usize {
        self.stream.as_ref().unwrap().as_raw_fd() as usize
    }
}

const MAX_BUFF_SIZE: usize = 4096;
const MAX_HEADER_SIZE: usize = 8;

const TOTAL_BUFF_SIZE: usize = MAX_HEADER_SIZE + MAX_BUFF_SIZE;

#[cfg(feature = "redis_server")]
pub fn redis_server() -> Result<(), std::io::Error> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();

    let address: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 8080));

    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;

    socket.bind(&address.into())?;
    socket.listen(128)?;

    let listener: TcpListener = socket.into();

    let mut socket_connections: Vec<Option<SocketConnection>> = vec![];
    let mut poll_args: Vec<PollFd> = vec![];

    let fd = listener.as_raw_fd();
    let bfd = unsafe { BorrowedFd::borrow_raw(fd) };

    loop {
        poll_args.clear();
        let pfd = PollFd::new(bfd, PollFlags::POLLIN);

        poll_args.push(pfd);

        for connection in socket_connections.iter() {
            if connection.is_none() {
                println!("NONE CONNECTIONS");
                continue;
            }

            let connection = connection.as_ref().unwrap();

            let mut event = PollFlags::POLLERR;

            if connection.want_read {
                event |= PollFlags::POLLIN;
            }

            if connection.want_write {
                event |= PollFlags::POLLOUT;
            }

            println!("EVENT: {event:?}");

            let fd = connection.stream.as_ref().unwrap().as_raw_fd();
            let bfd = unsafe { BorrowedFd::borrow_raw(fd) };
            let pfd = PollFd::new(bfd, event);

            poll_args.push(pfd);
        }

        let rv = poll(&mut poll_args, PollTimeout::NONE);

        match rv {
            Ok(pfd) => {
                println!("POLL RV {:?}", rv);
                if pfd < 0 {
                    println!("POLLFDS are ZERO");
                    continue;
                }
            }

            Err(e) => {
                println!("Failed get pollfds");
                continue;
            }
        }

        if poll_args[0].any().unwrap() {
            let new_conn = match handle_accept(&listener) {
                Some(conn) => conn,
                None => continue,
            };

            let fd = new_conn.get_fd();

            println!("ACCEPTED NEW CONNECTION: {fd}");

            if socket_connections.len() < fd {
                socket_connections.resize_with(fd + 1, Default::default);
                socket_connections[fd] = Some(new_conn);
            }

            println!("Connections len: {}", socket_connections.len());
        }

        for pdf_id in 1..poll_args.len() {
            let pfd = &poll_args[pdf_id];
            let conn = &mut socket_connections[pfd.as_fd().as_raw_fd() as usize];

            if conn.is_none() {
                continue;
            }

            println!("PFD IN: {pdf_id}");

            let mut connection = conn.as_mut().unwrap();

            println!("{:?}", pfd.events());

            match pfd.revents() {
                Some(event) => {
                    if event.intersects(PollFlags::POLLIN) {
                        handle_read(&mut connection);
                    }
                    if event.intersects(PollFlags::POLLOUT) {
                        handle_write(&mut connection);
                    }
                    if event.intersects(PollFlags::POLLERR) || connection.want_close {
                        conn.take();
                    }
                }
                None => todo!("TOTOTOTO"),
            }
        }
    }

    Ok(())
}

// #[cfg(feature = "redis_client")]
pub fn redis_client() -> Result<(), Error> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 8080));

    let s_conn = socket.connect(&address.into());

    let _ = match s_conn {
        Ok(connection) => connection,
        Err(_) => panic!("PANIC"),
    };

    let mut stream: TcpStream = socket.into();

    let mut clients: Vec<String> = vec![];
    let msg1 = "HELLO from client 1".to_string();
    let msg2 = "HELLO from client 2".to_string();
    let msg3 = "HELLO from client 3".to_string();

    clients.push(msg1);
    clients.push(msg2);
    clients.push(msg3);

    loop {
        println!("CLIENTS LENGHT {:?}", clients);

        for q in clients.iter() {
            println!("SEND MESSAGAE TO SERVER");
            let size = write_message(&mut stream, q);

            if size == 0 {
                println!("Client can not send data");
            }
        }

        for val in 0..clients.len() {
            let rv = read_message(&mut stream);
            println!("Server send back {:?} ", rv.as_str());
        }
    }

    Ok(())
}

pub fn handle_accept(listener: &TcpListener) -> Option<SocketConnection> {
    let connection = match listener.accept() {
        Ok(v) => v,
        Err(e) => {
            println!("FAILED TO GET CONNECTION {:?}", e);
            return None;
        }
    };

    let _ = listener.set_nonblocking(true);

    let new_conn: SocketConnection = SocketConnection {
        stream: Some(connection.0),
        want_read: true,
        want_write: false,
        want_close: false,
        incoming_data: vec![],
        outgoing_data: vec![],
    };

    Some(new_conn)
}

pub fn handle_read(conn: &mut SocketConnection) -> usize {
    let mut buff: [u8; TOTAL_BUFF_SIZE] = [0; TOTAL_BUFF_SIZE];

    let rv: usize = match conn.stream.as_ref().unwrap().read(&mut buff) {
        Ok(0) => {
            conn.want_close = true;
            println!("HANDLE READ O SIZE");
            return 0;
        }
        Ok(v) => {
            buff_append(&mut conn.incoming_data, &buff, v);
            println!("READ FROM CLIENT {v}");

            while try_one_request(conn) {}

            if conn.outgoing_data.len() > 0 {
                conn.want_write = true;
                conn.want_read = false;

                return handle_write(conn);
            }

            v
        }
        Err(e) => {
            println!("HANDLE READ ERROR: {:?}", e);
            conn.want_close = true;
            return 0;
        }
    };

    println!("HANDLE READ, {rv}");

    rv
}

pub fn handle_write(conn: &mut SocketConnection) -> usize {
    let wr = match conn.stream.as_ref().unwrap().write(&conn.outgoing_data) {
        Ok(0) => {
            conn.want_close = true;
            println!("HANDLE WRITE O SIZE");
            return 0;
        }
        Ok(v) => {
            if conn.outgoing_data.len() == 0 {
                conn.want_read = true;
                conn.want_write = false;
            }

            buff_consume(&mut conn.outgoing_data, v);

            println!("SERVER WRITE TO BUFF {v}");
            v
        }
        Err(e) => {
            println!("HANDLE READ ERROR: {:?}", e);
            conn.want_close = true;
            return 0;
        }
    };

    wr
}

fn read_full(stream: &mut TcpStream, buff: &mut [u8], n: usize) -> usize {
    // assert!(n > buff.len());
    let mut total = 0;
    while total < n {
        let read_value = match stream.read(&mut buff[total..n]) {
            Ok(0) => {
                return 0;
            }
            Ok(size) => size,
            Err(e) if e.kind() == ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => {
                println!("{:?}", e);
                break;
            }
        };

        total += read_value;
    }

    total
}

fn write_all(stream: &mut TcpStream, buff: &[u8], n: usize) -> usize {
    // assert!(n > buff.len());

    let mut total = 0;
    while total < n {
        let write_value = match stream.write(&buff[total..n]) {
            Ok(0) => {
                return 0;
            }
            Ok(size) => size,
            Err(e) if e.kind() == ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => {
                println!("Failed to write to stream {:?}", e);
                break;
            }
        };

        total += write_value;
    }

    total
}

pub fn write_message(stream: &mut TcpStream, message: &str) -> usize {
    let mut send_buffer: [u8; TOTAL_BUFF_SIZE] = [0; TOTAL_BUFF_SIZE];

    let message_len = message.len();
    let total_len = MAX_HEADER_SIZE + message_len;

    send_buffer[0..MAX_HEADER_SIZE].copy_from_slice(&message_len.to_ne_bytes());
    send_buffer[MAX_HEADER_SIZE..total_len].copy_from_slice(&message.as_bytes());

    let write_size = write_all(stream, &mut send_buffer, total_len);

    println!("{write_size}");

    write_size
}

pub fn read_message(stream: &mut TcpStream) -> String {
    let mut read_buffer: [u8; TOTAL_BUFF_SIZE] = [0; TOTAL_BUFF_SIZE];

    let _ = read_full(
        stream,
        &mut read_buffer[0..MAX_HEADER_SIZE],
        MAX_HEADER_SIZE,
    );

    let msg_len = get_message_len(&mut read_buffer);

    let read_size = read_full(
        stream,
        &mut read_buffer[MAX_HEADER_SIZE..MAX_HEADER_SIZE + msg_len],
        msg_len,
    );

    let message = match str::from_utf8(&read_buffer[MAX_HEADER_SIZE..MAX_HEADER_SIZE + read_size]) {
        Ok(msg) => msg,
        Err(e) => {
            println!("Failed to parse message buffer {:?}", e);
            ""
        }
    };

    String::from(message)
}

pub fn get_message_len(bytes: &[u8]) -> usize {
    let mut bytes_copy: [u8; MAX_HEADER_SIZE] = [0; MAX_HEADER_SIZE];
    bytes_copy.clone_from_slice(&bytes[0..MAX_HEADER_SIZE]);

    let msg_size = usize::from_ne_bytes(bytes_copy);
    msg_size
}

pub fn buff_append(buff: &mut Vec<u8>, data: &[u8], msg_len: usize) {
    buff.extend_from_slice(&data[0..msg_len]);
}

pub fn buff_consume(buff: &mut Vec<u8>, len: usize) {
    println!("BUFF LEN {}", buff.len());
    buff.drain(0..len);
}

pub fn try_one_request(connection: &mut SocketConnection) -> bool {
    if connection.incoming_data.len() < MAX_HEADER_SIZE {
        println!("INCOMING DOES NOT HAVE HEADER 8 BYTES");
        return false;
    }

    let msg_len = get_message_len(&connection.incoming_data);

    if msg_len > MAX_BUFF_SIZE {
        println!("MSG TOO LONG");
        connection.want_close = true;
        return false;
    }

    if MAX_HEADER_SIZE + msg_len > connection.incoming_data.len() {
        println!("INCOMING DATA TOO LONG 2");
        return false;
    }

    println!("INCOMING DATA {:?}", connection.incoming_data);

    let msg = str::from_utf8(&connection.incoming_data[MAX_HEADER_SIZE..MAX_HEADER_SIZE + msg_len])
        .unwrap();

    println!("Client says: {} {:?}", msg_len, msg);

    buff_append(
        &mut connection.outgoing_data,
        &msg_len.to_ne_bytes(),
        MAX_HEADER_SIZE,
    );

    buff_append(&mut connection.outgoing_data, &msg.as_bytes(), msg_len);

    println!("MSG from CLIENT: {msg}");

    buff_consume(&mut connection.incoming_data, MAX_HEADER_SIZE + msg_len);

    return true;
}
