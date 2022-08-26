use anyhow::Error;
use argh::FromArgs;
use async_stream::stream;
use futures_util::{pin_mut, FutureExt};
use std::{collections::HashMap, convert::TryInto, sync::Arc, time::Duration};
use superconsole::{
    components::splitting::{Split, SplitKind},
    state,
    style::Stylize,
    Component, Dimensions, Direction, DrawMode, Line, Span, State, SuperConsole,
};
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    time,
};
use tokio_stream::{Stream, StreamExt};

#[derive(FromArgs)]
/// Throughput tester
struct Args {
    /// address to bind too
    #[argh(option)]
    bind: std::net::SocketAddr,

    /// target of loopback service
    #[argh(option)]
    target: std::net::SocketAddr,
}

#[derive(Default)]
struct Stat {
    sent: usize,
    missed: usize,
}

#[derive(Debug)]
struct RunComponent {
    id: usize,
    hertz: f32,
    byte_size: usize,
}

impl Component for RunComponent {
    fn draw_unchecked(
        &self,
        state: &State,
        _dimensions: Dimensions,
        _mode: DrawMode,
    ) -> Result<Vec<Line>, Error> {
        let stat = state.get::<HashMap<usize, Stat>>()?.get(&self.id);
        let mut messages = vec![];
        messages.push(vec![format!("{}. Rate: {}hz", self.id, self.hertz)].try_into()?);
        messages.push(vec![format!("   Packet Size: {} bytes", self.byte_size)].try_into()?);
        match stat {
            Some(stat) => {
                let sent = Span::new_styled(format!("   Sent: {} ", stat.sent).to_owned().blue())?;
                let missed =
                    Span::new_styled(format!("Missed: {} ", stat.missed).to_owned().yellow())?;
                messages.push(superconsole::line!(sent, missed));
            }
            None => {
                let not = Span::new_styled("   Not Started".to_owned().red().bold())?;
                messages.push(superconsole::line!(not));
            }
        }
        Ok(messages)
    }
}

struct Run<A: ToSocketAddrs> {
    socket: Arc<UdpSocket>,
    addr: A,
    hertz: f32,
    timeout: Duration,
    byte_size: usize,
}

impl<A: ToSocketAddrs + Clone> Run<A> {
    pub fn new(socket: Arc<UdpSocket>, addr: A, hertz: f32, byte_size: usize) -> Run<A> {
        let timeout = Duration::from_secs_f32(1.0 / hertz);
        Run {
            socket,
            addr,
            hertz,
            timeout,
            byte_size,
        }
    }
    fn start(&self) -> impl Stream<Item = Result<bool, Error>> + '_ {
        stream! {
            for i in 0..100 {
                let msg = send_msg(&self.socket, self.addr.clone(), i, self.timeout, self.byte_size).await;
                yield msg
            }
        }
    }
}

async fn send_msg<A: ToSocketAddrs>(
    socket: &UdpSocket,
    addr: A,
    num: usize,
    timeout: Duration,
    byte_size: usize,
) -> Result<bool, Error> {
    let window = time::sleep(timeout);

    // build a message of byte_size
    let mut msg: Vec<u8> = num.to_le_bytes().into();
    for _ in 0..byte_size - num.to_le_bytes().len() {
        msg.push(0xff)
    }

    let send_recv = async {
        socket.send_to(&msg, addr).await?;
        let mut buf = [0; 8];
        socket.recv(&mut buf).await?;
        let ret_num = usize::from_le_bytes(buf[0..8].try_into().unwrap());
        if num != ret_num {
            anyhow::bail!("oh no");
        }
        Ok(ret_num)
    }
    .fuse();

    tokio::pin!(window, send_recv);
    let mut recv = false;
    loop {
        tokio::select! {
            () = &mut window => {
                return Ok(recv);
            },
            _ = &mut send_recv => {
                recv = true;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Args = argh::from_env();

    let addr = args.target;
    let socket = Arc::new(UdpSocket::bind(args.bind).await?);

    let runs = vec![
        Run::new(socket.clone(), addr, 4.0, 50),
        Run::new(socket.clone(), addr, 4.0, 100),
        Run::new(socket.clone(), addr, 4.0, 200),
        Run::new(socket.clone(), addr, 8.0, 50),
        Run::new(socket.clone(), addr, 8.0, 100),
        Run::new(socket.clone(), addr, 8.0, 200),
        Run::new(socket.clone(), addr, 16.0, 50),
        Run::new(socket.clone(), addr, 16.0, 100),
        Run::new(socket.clone(), addr, 16.0, 200),
    ];

    let run_components = runs
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            Box::new(RunComponent {
                id: idx,
                hertz: r.hertz,
                byte_size: r.byte_size,
            }) as Box<dyn superconsole::Component>
        })
        .collect();

    let mut console = SuperConsole::new(Box::new(Split::new(
        run_components,
        Direction::Vertical,
        SplitKind::Adaptive,
    )))
    .ok_or_else(|| anyhow::anyhow!("Not a TTY"))?;

    let mut state: HashMap<usize, Stat> = Default::default();

    for (idx, r) in runs.iter().enumerate() {
        let stream = r.start();
        pin_mut!(stream);
        while let Some(Ok(msg)) = stream.next().await {
            let stat = state.entry(idx).or_default();
            if !msg {
                stat.missed += 1;
            }
            stat.sent += 1;

            let state = state!(&state);
            console.render(&state)?;
        }
    }
    Ok(())
}
