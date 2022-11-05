use nom::{
    branch::alt,
    bytes::streaming::{tag, take},
    combinator::{map_res, verify},
    multi::count,
    number::streaming::{be_u16, be_u32, u8},
    sequence::tuple,
    IResult,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Bound,
    str,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, Mutex},
    time,
};
use tracing::{debug, error, info, instrument, warn, Instrument};

#[derive(Debug, Clone)]
struct CameraState {
    road: u16,
    mile: u16,
    limit: u16,
}

#[derive(Debug)]
struct DispatcherState {
    ticket_listener: mpsc::Receiver<Ticket>,
    ticketed_days: HashSet<(String, u32)>,
}

#[derive(Debug, Default)]
struct DispatcherListState {
    dispatcher_list: HashMap<u16, mpsc::Sender<Ticket>>,
    pending_tickets: HashMap<u16, Vec<Ticket>>,
}

#[derive(Debug)]
struct PlateReading {
    plate: String,
    timestamp: u32,
}

#[derive(Debug)]
struct PlateReport {
    camera_state: CameraState,
    plate_reading: PlateReading,
}

#[derive(Debug)]
struct Ticket {
    plate: String,
    road: u16,
    mile1: u16,
    timestamp1: u32,
    mile2: u16,
    timestamp2: u32,
    speed: u16,
}

#[derive(Debug)]
enum Message {
    Plate(PlateReading),
    WantHeartbeat { interval: u32 },
    IAmCamera { road: u16, mile: u16, limit: u16 },
    IAmDispatcher { roads: Vec<u16> },
}

fn message_string(input: &[u8]) -> IResult<&[u8], &str, ()> {
    let (input, length) = u8(input)?;
    map_res(
        verify(take(length), |string: &[u8]| string.is_ascii()),
        str::from_utf8,
    )(input)
}

fn plate_message(input: &[u8]) -> IResult<&[u8], Message, ()> {
    let (input, _) = tag([0x20])(input)?;
    let (input, (plate, timestamp)) = tuple((message_string, be_u32))(input)?;

    Ok((
        input,
        Message::Plate(PlateReading {
            plate: plate.to_owned(),
            timestamp,
        }),
    ))
}

fn want_heartbeat_message(input: &[u8]) -> IResult<&[u8], Message, ()> {
    let (input, _) = tag([0x40])(input)?;
    let (input, interval) = be_u32(input)?;

    Ok((input, Message::WantHeartbeat { interval }))
}

fn i_am_camera_message(input: &[u8]) -> IResult<&[u8], Message, ()> {
    let (input, _) = tag([0x80])(input)?;
    let (input, (road, mile, limit)) = tuple((be_u16, be_u16, be_u16))(input)?;

    Ok((input, Message::IAmCamera { road, mile, limit }))
}

fn i_am_dispatcher_message(input: &[u8]) -> IResult<&[u8], Message, ()> {
    let (input, _) = tag([0x81])(input)?;
    let (input, numroads) = u8(input)?;
    let (input, roads) = count(be_u16, numroads.into())(input)?;

    Ok((input, Message::IAmDispatcher { roads }))
}

fn parse_message(input: &[u8]) -> IResult<&[u8], Message, ()> {
    alt((
        plate_message,
        want_heartbeat_message,
        i_am_camera_message,
        i_am_dispatcher_message,
    ))(input)
}

#[derive(Debug)]
enum ReadError {
    ConnectionClosed,
    IOError(std::io::Error),
    ParseError(nom::Err<()>),
}

async fn read_message<R: AsyncRead + Unpin>(
    mut reader: R,
    input: &mut Vec<u8>,
) -> Result<Message, ReadError> {
    let mut buf = [0; 64];
    loop {
        match parse_message(input) {
            Err(nom::Err::Incomplete(_)) => {
                let read = reader.read(&mut buf).await.map_err(ReadError::IOError)?;
                if read == 0 {
                    return Err(ReadError::ConnectionClosed);
                }
                input.extend_from_slice(&buf[..read]);
            }
            res => {
                let (remaining_input, message) = res.map_err(ReadError::ParseError)?;
                *input = remaining_input.to_owned();
                return Ok(message);
            }
        }
    }
}

fn encode_error(error: &str) -> Vec<u8> {
    warn!(error, "sending error to client");
    let mut encoded = vec![0x10, error.len() as u8];
    encoded.extend_from_slice(error.as_bytes());
    encoded
}

fn encode_ticket(ticket: Ticket) -> Vec<u8> {
    let mut encoded = vec![0x21, ticket.plate.len() as u8];
    encoded.extend(ticket.plate.as_bytes());
    encoded.extend(ticket.road.to_be_bytes());
    encoded.extend(ticket.mile1.to_be_bytes());
    encoded.extend(ticket.timestamp1.to_be_bytes());
    encoded.extend(ticket.mile2.to_be_bytes());
    encoded.extend(ticket.timestamp2.to_be_bytes());
    encoded.extend(ticket.speed.to_be_bytes());
    encoded
}

async fn send_ticket<W: AsyncWrite + Unpin>(
    dispatcher_state: &mut Option<DispatcherState>,
    mut writer: W,
    ticket: Ticket,
) {
    let ticketed_days = &mut dispatcher_state
        .as_mut()
        .expect("dispatcher was not set")
        .ticketed_days;
    let day1 = ticket.timestamp1 / 86400;
    let day1_key = (ticket.plate.clone(), day1);
    let is_day1_ticketed = ticketed_days.contains(&day1_key);
    let day2 = ticket.timestamp2 / 86400;
    let day2_key = (ticket.plate.clone(), day2);
    let is_day2_ticketed = ticketed_days.contains(&day2_key);
    debug!(?ticket, day1, day2, "received ticket");
    if !is_day1_ticketed && !is_day2_ticketed {
        debug!(?ticket, day1, day2, "sending ticket");
        if let Err(error) = writer.write(&encode_ticket(ticket)).await {
            error!(?error, "failed to send ticket");
        }
        ticketed_days.insert(day1_key);
        ticketed_days.insert(day2_key);
    }
}

struct CameraComparison {
    plate: String,
    road: u16,
    limit: u16,
    mile1: u16,
    timestamp1: u32,
    mile2: u16,
    timestamp2: u32,
}

async fn check_speed(
    dispatcher_list_state: &Arc<Mutex<DispatcherListState>>,
    CameraComparison {
        plate,
        road,
        limit,
        mile1,
        timestamp1,
        mile2,
        timestamp2,
    }: CameraComparison,
) {
    let distance = mile2.abs_diff(mile1);
    let time = timestamp2.abs_diff(timestamp1);
    let speed = f64::round((distance as f64 * 3600.0) / time as f64) as u16;
    debug!(plate, speed, limit, "checking speed");
    if speed > limit {
        let ticket = Ticket {
            plate,
            road,
            mile1,
            timestamp1,
            mile2,
            timestamp2,
            speed: speed * 100,
        };
        let mut dispatcher_list_state = dispatcher_list_state.lock().await;
        match dispatcher_list_state.dispatcher_list.get(&road) {
            Some(dispatcher) => {
                debug!(road, "sending ticket to dispatcher");
                if (dispatcher.send(ticket).await).is_err() {
                    debug!(road, "dispatcher has disconnected");
                    dispatcher_list_state.dispatcher_list.remove(&road);
                }
            }
            None => {
                debug!(road, "storing ticket for when dispatcher connects");
                let pending = dispatcher_list_state
                    .pending_tickets
                    .entry(road)
                    .or_default();
                pending.push(ticket);
            }
        }
    }
}

#[instrument]
pub async fn run() {
    let dispatcher_list_state = Arc::new(Mutex::new(Default::default()));
    let (plate_reporter, mut plate_listener) = mpsc::channel(100);
    let listener = TcpListener::bind("0.0.0.0:10006")
        .await
        .expect("failed to bind socket");
    let dls = Arc::clone(&dispatcher_list_state);
    tokio::spawn(async move {
        let mut plate_history: HashMap<_, HashMap<String, BTreeMap<u32, u16>>> = HashMap::new();
        loop {
            let plate: PlateReport = plate_listener
                .recv()
                .await
                .expect("plate channel has been closed");
            let road_history = plate_history.entry(plate.camera_state.road).or_default();
            let car_history = road_history
                .entry(plate.plate_reading.plate.clone())
                .or_default();
            car_history.insert(plate.plate_reading.timestamp, plate.camera_state.mile);

            if let Some((prev_timestamp, prev_mile)) = car_history
                .range(..plate.plate_reading.timestamp)
                .next_back()
            {
                check_speed(
                    &dls,
                    CameraComparison {
                        plate: plate.plate_reading.plate.clone(),
                        road: plate.camera_state.road,
                        limit: plate.camera_state.limit,
                        mile1: *prev_mile,
                        timestamp1: *prev_timestamp,
                        mile2: plate.camera_state.mile,
                        timestamp2: plate.plate_reading.timestamp,
                    },
                )
                .await;
            }
            if let Some((next_timestamp, next_mile)) = car_history
                .range((
                    Bound::Excluded(plate.plate_reading.timestamp),
                    Bound::Unbounded,
                ))
                .next()
            {
                check_speed(
                    &dls,
                    CameraComparison {
                        plate: plate.plate_reading.plate,
                        road: plate.camera_state.road,
                        limit: plate.camera_state.limit,
                        mile1: plate.camera_state.mile,
                        timestamp1: plate.plate_reading.timestamp,
                        mile2: *next_mile,
                        timestamp2: *next_timestamp,
                    },
                )
                .await;
            }
        }
    });
    loop {
        let (mut socket, addr) = listener
            .accept()
            .await
            .expect("failed to accept connection");
        let dispatcher_list_state = Arc::clone(&dispatcher_list_state);
        let plate_reporter = plate_reporter.clone();
        tokio::spawn(
            async move {
                info!("accepted connection");
                let (mut reader, mut writer) = socket.split();
                let mut input = Vec::new();
                let mut camera_state: Option<CameraState> = None;
                let mut dispatcher_state: Option<DispatcherState> = None;
                let mut heartbeat: Option<time::Interval> = None;

                loop {
                    tokio::select! {
                        message = read_message(&mut reader, &mut input) => {
                            if message.is_ok() {
                                info!(?message, "parsed message");
                            }
                            match message {
                                Ok(Message::Plate(plate_reading)) => match camera_state {
                                    Some(ref camera_state) => plate_reporter
                                        .send(PlateReport {
                                            camera_state: camera_state.clone(),
                                            plate_reading,
                                        })
                                        .await
                                        .expect("failed to report plate"),
                                    None => {
                                        let _ = writer
                                            .write(&encode_error("client is not a camera"))
                                            .await;
                                        break;
                                    }
                                },
                                Ok(Message::WantHeartbeat { interval }) => {
                                    if interval != 0
                                        && heartbeat
                                            .replace(time::interval(Duration::from_millis(
                                                u64::from(interval) * 100,
                                            )))
                                            .is_some()
                                    {
                                        let _ = writer
                                            .write(&encode_error("set heartbeat twice"))
                                            .await;
                                        break;
                                    }
                                }
                                Ok(Message::IAmCamera { road, mile, limit }) => {
                                    let prev_data =
                                        camera_state.replace(CameraState { road, mile, limit });
                                    if prev_data.is_some() || dispatcher_state.is_some() {
                                        let _ = writer
                                            .write(&encode_error("set client data twice"))
                                            .await;
                                        break;
                                    }
                                }
                                Ok(Message::IAmDispatcher { roads, .. }) => {
                                    if dispatcher_state.is_some() || camera_state.is_some() {
                                        let _ = writer
                                            .write(&encode_error("set client data twice"))
                                            .await;
                                        break;
                                    }
                                    let (ticket_reporter, ticket_listener) = mpsc::channel(10);
                                    dispatcher_state = Some(DispatcherState {
                                        ticket_listener,
                                        ticketed_days: HashSet::new(),
                                    });
                                    let mut dispatcher_list_state =
                                        dispatcher_list_state.lock().await;
                                    for road in roads.into_iter() {
                                        dispatcher_list_state
                                            .dispatcher_list
                                            .entry(road)
                                            .or_insert_with(|| ticket_reporter.clone());
                                        if let Some(tickets) =
                                            dispatcher_list_state.pending_tickets.remove(&road)
                                        {
                                            for ticket in tickets.into_iter() {
                                                send_ticket(
                                                    &mut dispatcher_state,
                                                    &mut writer,
                                                    ticket,
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                }
                                Err(ReadError::ConnectionClosed) => {
                                    info!("connection closed by client");
                                    break;
                                }
                                Err(error) => {
                                    error!(?error, "failed to read message");
                                    let _ =
                                        writer.write(&encode_error("failed to read message")).await;
                                    break;
                                }
                            }
                        }
                        Some(ticket) = async {
                            dispatcher_state.as_mut()?.ticket_listener.recv().await
                        } => send_ticket(&mut dispatcher_state, &mut writer, ticket).await,
                        Some(_) = async {
                            Some(heartbeat.as_mut()?.tick().await)
                        } => {
                            let _ = writer.write(&[0x41]).await;
                        }
                    }
                }
                if let Some(DispatcherState {
                    mut ticket_listener,
                    ..
                }) = dispatcher_state
                {
                    ticket_listener.close();
                }
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
