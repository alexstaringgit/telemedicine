#![allow(unused_variables)]
extern crate rand;
extern crate bytes;
extern crate byteorder;
extern crate futures;
extern crate tokio_io;
extern crate tokio_core;
extern crate env_logger;
extern crate serde;
extern crate serde_json;
#[macro_use] extern crate serde_derive;

#[macro_use]
extern crate actix;
extern crate actix_web;

use std::time::Instant;

use actix::*;
use actix_web::*;

mod codec;
mod server;
mod session;

/// This is our websocket route state, this state is shared with all route instances
/// via `HttpContext::state()`
struct WsChatSessionState {
    addr: SyncAddress<server::ChatServer>,
}

/// Entry point for our route
fn chat_route(req: HttpRequest<WsChatSessionState>) -> Result<HttpResponse> {
    ws::start(
        req,
        WsChatSession {
            id: 0,
            hb: Instant::now(),
            room: "Main".to_owned(),
            name: None})
}

struct WsChatSession {
    /// unique session id
    id: usize,
    /// Client must send ping at least once per 10 seconds, otherwise we drop connection.
    hb: Instant,
    /// joined room
    room: String,
    /// peer name
    name: Option<String>,
}

impl Actor for WsChatSession {
    type Context = ws::WebsocketContext<Self, WsChatSessionState>;

    /// Method is called on actor start.
    /// We register ws session with ChatServer
    fn started(&mut self, ctx: &mut Self::Context) {
        // register self in chat server. `AsyncContext::wait` register
        // future within context, but context waits until this future resolves
        // before processing any other events.
        // HttpContext::state() is instance of WsChatSessionState, state is shared across all
        // routes within application
        let subs = ctx.sync_subscriber();
        ctx.state().addr.call(
            self, server::Connect{addr: subs}).then(
            |res, act, ctx| {
                match res {
                    Ok(Ok(res)) => act.id = res,
                    // something is wrong with chat server
                    _ => ctx.stop(),
                }
                fut::ok(())
            }).wait(ctx);
    }

    fn stopping(&mut self, ctx: &mut Self::Context) -> bool {
        // notify chat server
        ctx.state().addr.send(server::Disconnect{id: self.id});
        true
    }
}

/// Handle messages from chat server, we simply send it to peer websocket
impl Handler<session::Message> for WsChatSession {
    type Result = ();

    fn handle(&mut self, msg: session::Message, ctx: &mut Self::Context) {
        ctx.text(&msg.0);
    }
}

/// WebSocket message handler
impl Handler<ws::Message> for WsChatSession {
    type Result = ();

    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        println!("WEBSOCKET MESSAGE: {:?}", msg);
        match msg {
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Pong(msg) => self.hb = Instant::now(),
            ws::Message::Text(text) => {
                let m = text.trim();
                // we check for /sss type of messages
                if m.starts_with('/') {
                    let v: Vec<&str> = m.splitn(2, ' ').collect();
                    match v[0] {
                        "/list" => {
                            // Send ListRooms message to chat server and wait for response
                            println!("List rooms");
                            ctx.state().addr.call(self, server::ListRooms).then(|res, _, ctx| {
                                match res {
                                    Ok(Ok(rooms)) => {
                                        for room in rooms {
                                            ctx.text(&room);
                                        }
                                    },
                                    _ => println!("Something is wrong"),
                                }
                                fut::ok(())
                            }).wait(ctx)
                            // .wait(ctx) pauses all events in context,
                            // so actor wont receive any new messages until it get list
                            // of rooms back
                        },
                        "/join" => {
                            if v.len() == 2 {
                                self.room = v[1].to_owned();
                                ctx.state().addr.send(
                                    server::Join{id: self.id, name: self.room.clone()});

                                ctx.text("joined");
                            } else {
                                ctx.text("!!! room name is required");
                            }
                        },
                        "/name" => {
                            if v.len() == 2 {
                                self.name = Some(v[1].to_owned());
                            } else {
                                ctx.text("!!! name is required");
                            }
                        },
                        _ => ctx.text(&format!("!!! unknown command: {:?}", m)),
                    }
                } else {
                    let msg = if let Some(ref name) = self.name {
                        format!("{}: {}", name, m)
                    } else {
                        m.to_owned()
                    };
                    // send message to chat server
                    ctx.state().addr.send(
                        server::Message{id: self.id,
                                        msg: msg,
                                        room: self.room.clone()})
                }
            },
            ws::Message::Binary(bin) =>
                println!("Unexpected binary"),
            ws::Message::Closed | ws::Message::Error => {
                ctx.stop();
            }
            _ => (),
        }
    }
}

fn main() {
    let _ = env_logger::init();
    let sys = actix::System::new("websocket-example");

    // Start chat server actor in separate thread
    let server: SyncAddress<_> =
        Arbiter::start(|_| server::ChatServer::default());

    // Start tcp server in separate thread
    let srv = server.clone();
    Arbiter::new("tcp-server").send::<msgs::Execute>(
        msgs::Execute::new(move || {
            session::TcpServer::new("127.0.0.1:12345", srv);
            Ok(())
        }));

    // Create Http server with websocket support
    let addr = HttpServer::new(
        move || {
            // Websocket sessions state
            let state = WsChatSessionState { addr: server.clone() };

            Application::with_state(state)
                // redirect to websocket.html
                .resource("/", |r| r.method(Method::GET).f(|_| {
                    httpcodes::HTTPFound
                        .build()
                        .header("LOCATION", "/static/websocket.html")
                        .finish()
                }))
                // websocket
                .resource("/ws/", |r| r.route().f(chat_route))
                // static resources
                .handler("/static/", fs::StaticFiles::new("static/", true))
        })
        .bind("127.0.0.1:8080").unwrap()
        .start();

    println!("Started http server: 127.0.0.1:8080");
    let _ = sys.run();
}
