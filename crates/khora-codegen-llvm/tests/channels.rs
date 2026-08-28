#![cfg(feature = "llvm")]

//! Bounded channels, end to end.
//!
//! `docs/design/channels.md` says why one exists: a handler may not capture
//! anything writable, so a capability over a resource that cannot be shared
//! needs one fiber to own the resource and the rest to ask it. What these pin
//! is the part a program can see — values arrive in order, a full channel
//! stops the sender, a close releases everybody, and nothing leaks.
//!
//! The last of those is what `khora_live_count` is for. A channel holds
//! references to values the runtime cannot see the type of, so a mistake in the
//! reference counting here does not show up as a wrong answer — it shows up as
//! a program that slowly runs out of memory, which no test asserting on output
//! would catch.

mod harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

struct Ran {
    stdout: String,
    code: Option<i32>,
}

fn run(name: &str, source: &str) -> Ran {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, dir.join("main.kh"), source.to_string());
    let root = SourceRoot::new(&db, vec![file]);

    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        panic!("compiling `{name}` failed:\n  {}\n\n{source}", messages.join("\n  "));
    }

    let ran = Command::new(&exe).output().expect("the program should run");
    Ran {
        stdout: String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n"),
        code: ran.status.code(),
    }
}

/// Everything a channel test needs and nothing it does not: `Option`, `Fiber`,
/// a nursery, and the live-object count.
const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

pub type Option<A> = | None | Some(A);
pub trait Share {}
impl String { fn byte_length(self) -> Int; }

pub type Channel<A>;
impl<A> Share for Channel<A> {}
impl<A: Share> Channel<A> {
  fn bounded(capacity: Int) -> Channel<A>;
  fn dropping(capacity: Int) -> Channel<A>;
  fn sliding(capacity: Int) -> Channel<A>;
  fn send(self, value: A) -> Bool;
  fn receive(self) -> Option<A>;
  fn poll(self) -> Option<A>;
  fn close(self) -> ();
  fn depth(self) -> Int;
}
// `Int` and `String` are shareable inherently -- a value with no interior is
// safe to hold twice -- and the orphan rule refuses an impl for them here
// anyway, since neither is declared in this module.

pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> {
  fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>;
  fn join(self) -> A raises 'r;
  fn wait(self) -> ();
  fn cancel(self) -> ();
}
impl<A, 'r> Share for Fiber<A, 'r> {}

pub type Fibers;
impl Share for Fibers {}
impl Fibers {
  fn open() -> Fibers;
  fn adopt<'er>(self, child: Fiber<(), 'er>) -> ();
  fn wait(self) -> ();
}
pub effect Nursery { adopt: (Fiber<(), 'er>) -> (), }
";

/// One fiber puts values in, another takes them out, and the order survives.
#[test]
fn values_cross_between_fibers_in_order() {
    let ran = run(
        "channel_order",
        &format!(
            "{PRELUDE}
fn produce(out: Channel<Int>) -> () {{
  let mut i = 1;
  while i <= 5 {{
    Channel::send(out, i);
    i = i + 1
  }};
  Channel::close(out);
}}

// The counting happens in `main`, after this returns, because a channel still
// in scope is a live object and would be counted as a leak.
fn drain() -> () {{
  let line = Channel::bounded(2);
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => produce(line)));
  }};
  let mut going = true;
  while going {{
    match Channel::receive(line) {{
      Option::Some(value) => print(value),
      Option::None => going = false,
    }}
  }};
  Fibers::wait(crew);
}}

fn main() -> Int {{
  drain();
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n3\n4\n5\n0\n", "in order, then nothing left alive");
    assert_eq!(ran.code, Some(0));
}

/// **The point of a bounded channel.** Capacity is one and five values are
/// sent, so the producer cannot have finished before the consumer started —
/// and it does finish, which is what says the wake arrived.
///
/// A capacity-one channel with the reader running only after the writer is
/// joined would deadlock, so this shape is the assertion: it terminates.
#[test]
fn a_full_channel_stops_the_sender() {
    let ran = run(
        "channel_backpressure",
        &format!(
            "{PRELUDE}
fn produce(out: Channel<Int>) -> () {{
  let mut i = 0;
  while i < 5 {{
    Channel::send(out, i);
    i = i + 1
  }};
  Channel::close(out);
}}

fn main() -> Int {{
  let line = Channel::bounded(1);
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => produce(line)));
  }};
  let mut total = 0;
  let mut going = true;
  while going {{
    match Channel::receive(line) {{
      Option::Some(value) => total = total + value,
      Option::None => going = false,
    }}
  }};
  Fibers::wait(crew);
  print(total);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "10\n", "0+1+2+3+4, with the sender waiting four times");
    assert_eq!(ran.code, Some(0));
}

/// Request in, reply out — the shape every capability over an owned resource
/// takes, and the one `packages/postgres` is built on.
#[test]
fn a_reply_channel_carries_an_answer_back() {
    let ran = run(
        "channel_reply",
        &format!(
            "{PRELUDE}
// No `impl Share for Ask`: the compiler can see what a record holds and
// decides for itself. Every field here is shareable, so the record is.
pub type Ask = {{ question: Int, reply: Channel<Int> }};

fn serve(requests: Channel<Ask>) -> () {{
  let mut going = true;
  while going {{
    match Channel::receive(requests) {{
      Option::Some(ask) => {{
        Channel::send(ask.reply, ask.question * 2);
        Channel::close(ask.reply);
      }},
      Option::None => going = false,
    }}
  }};
}}

fn request(requests: Channel<Ask>, question: Int) -> Int {{
  let reply = Channel::bounded(1);
  Channel::send(requests, {{ question: question, reply: reply }});
  match Channel::receive(reply) {{
    Option::Some(answer) => answer,
    Option::None => 0 - 1,
  }}
}}

fn converse() -> () {{
  let requests = Channel::bounded(4);
  let crew = Fibers::open();
  with {{ nursery: handler for Nursery {{ adopt: fn f => Fibers::adopt(crew, f) }} }} {{
    nursery.adopt(Fiber::spawn(fn () => serve(requests)));
  }};
  print(request(requests, 21));
  print(request(requests, 5));
  Channel::close(requests);
  Fibers::wait(crew);
}}

fn main() -> Int {{
  converse();
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "42\n10\n0\n", "two answers, and nothing left alive");
    assert_eq!(ran.code, Some(0));
}

/// Values already sent are still worth having, so a close drains before it
/// ends. A reader that stopped at the close would lose them.
#[test]
fn a_closed_channel_gives_up_what_is_still_in_it() {
    let ran = run(
        "channel_drain",
        &format!(
            "{PRELUDE}
fn main() -> Int {{
  let line = Channel::bounded(4);
  Channel::send(line, 1);
  Channel::send(line, 2);
  Channel::close(line);
  let mut going = true;
  while going {{
    match Channel::receive(line) {{
      Option::Some(value) => print(value),
      Option::None => going = false,
    }}
  }};
  print(9);
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "1\n2\n9\n");
    assert_eq!(ran.code, Some(0));
}

/// A boxed value in flight is owned by the queue. This is the case where a
/// reference-counting mistake shows up as a leak rather than a wrong answer,
/// which is why the count is the assertion and the text is only the evidence
/// that the value survived the crossing.
#[test]
fn a_boxed_value_survives_the_crossing_and_is_not_leaked() {
    let ran = run(
        "channel_boxed",
        &format!(
            "{PRELUDE}
fn width(text: Option<String>) -> Int {{
  match text {{
    Option::Some(value) => String::byte_length(value),
    Option::None => 0 - 1,
  }}
}}

fn cross() -> () {{
  let line = Channel::bounded(2);
  Channel::send(line, \"first\");
  Channel::send(line, \"second longer\");
  print(width(Channel::receive(line)));
  print(width(Channel::receive(line)));
  Channel::close(line);
}}

fn main() -> Int {{
  cross();
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "5\n13\n0\n", "both strings crossed whole, and neither leaked");
    assert_eq!(ran.code, Some(0));
}

/// A value nobody ever takes is released when the channel is, not leaked.
/// Abandoning one is the ordinary case at shutdown, so it must not be the case
/// that grows the heap.
#[test]
fn a_value_nobody_receives_is_released_with_the_channel() {
    let ran = run(
        "channel_abandoned",
        &format!(
            "{PRELUDE}
fn fill() -> () {{
  let line = Channel::bounded(4);
  Channel::send(line, \"abandoned\");
  Channel::send(line, \"also abandoned\");
  Channel::close(line);
}}

fn main() -> Int {{
  fill();
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n", "the channel and both strings are gone");
    assert_eq!(ran.code, Some(0));
}

/// A send with nowhere to put its value must not be the quietest possible
/// leak, so the value is released and the answer says what happened.
#[test]
fn sending_to_a_closed_channel_answers_false() {
    let ran = run(
        "channel_closed_send",
        &format!(
            "{PRELUDE}
fn refuse() -> () {{
  let line = Channel::bounded(2);
  Channel::close(line);
  if Channel::send(line, \"nowhere to go\") {{ print(1) }} else {{ print(0) }};
}}

fn main() -> Int {{
  refuse();
  // Zero, which says the string the send could not place was released rather
  // than kept -- the quietest possible leak, if it were wrong.
  print(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(ran.stdout, "0\n0\n", "refused, and the value was not leaked");
    assert_eq!(ran.code, Some(0));
}

/// **A dropping channel refuses, and says it refused.**
///
/// The whole reason to pick this one over `sliding` is that `send` answers
/// `false`, so the producer that must not stall can still count what it lost.
/// The values that survive are the *oldest* -- nothing already accepted is
/// disturbed -- which is the other half of the difference.
#[test]
fn a_dropping_channel_refuses_the_newest_and_says_so() {
    let ran = run(
        "channel_dropping",
        &format!(
            "{PRELUDE}
fn fill() -> () {{
  let line = Channel::dropping(2);
  let mut i = 1;
  let mut refused = 0;
  while i <= 5 {{
    if Channel::send(line, i) {{ }} else {{ refused = refused + 1 }};
    i = i + 1
  }};
  print(refused);
  print(Channel::depth(line));
  let mut going = true;
  while going {{
    match Channel::poll(line) {{
      Option::Some(value) => print(value),
      Option::None => going = false,
    }}
  }};
  Channel::close(line);
}}

fn main() -> Int {{
  fill();
  print(khora_live_count());
  0
}}
"
        ),
    );

    // Three refused, two kept, and the two kept are the first two.
    assert_eq!(ran.stdout, "3\n2\n1\n2\n0\n", "{}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// **A sliding channel accepts, and drops the oldest to make room.**
///
/// `send` answers `true` every time, which is the point: nothing was refused,
/// so there is nothing for a caller to handle. What is left is the newest
/// values, which is what a gauge or a last-known-position wants.
#[test]
fn a_sliding_channel_keeps_the_newest_and_never_refuses() {
    let ran = run(
        "channel_sliding",
        &format!(
            "{PRELUDE}
fn fill() -> () {{
  let line = Channel::sliding(2);
  let mut i = 1;
  let mut refused = 0;
  while i <= 5 {{
    if Channel::send(line, i) {{ }} else {{ refused = refused + 1 }};
    i = i + 1
  }};
  print(refused);
  print(Channel::depth(line));
  let mut going = true;
  while going {{
    match Channel::poll(line) {{
      Option::Some(value) => print(value),
      Option::None => going = false,
    }}
  }};
  Channel::close(line);
}}

fn main() -> Int {{
  fill();
  print(khora_live_count());
  0
}}
"
        ),
    );

    // Nothing refused, two kept, and the two kept are the last two.
    assert_eq!(ran.stdout, "0\n2\n4\n5\n0\n", "{}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// **A slid-out value is released**, which is the part no output can show.
///
/// A sliding channel drops a reference on every send past its capacity, and
/// getting that wrong is not a wrong answer -- it is a service that grows all
/// day. `khora_live_count` is the only thing that can see it, which is why
/// these values are strings rather than numbers.
#[test]
fn nothing_slid_out_of_a_channel_is_leaked() {
    let ran = run(
        "channel_sliding_release",
        &format!(
            "{PRELUDE}
fn fill() -> () {{
  let line = Channel::sliding(1);
  let mut i = 0;
  while i < 8 {{
    Channel::send(line, \"a value long enough not to be a small string\");
    i = i + 1
  }};
  print(Channel::depth(line));
  Channel::close(line);
}}

fn main() -> Int {{
  fill();
  print(khora_live_count());
  0
}}
"
        ),
    );

    // One left in the queue, and the close released it: nothing outlives `fill`.
    assert_eq!(ran.stdout, "1\n0\n", "{}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}

/// **`poll` does not wait**, and an empty channel is `None` rather than a hang.
///
/// The test that would deadlock if it were `receive`, which is the only way to
/// state the difference.
#[test]
fn polling_an_empty_channel_answers_at_once() {
    let ran = run(
        "channel_poll",
        &format!(
            "{PRELUDE}
fn look() -> () {{
  let line = Channel::bounded(4);
  match Channel::poll(line) {{
    Option::Some(_value) => print(1),
    Option::None => print(0),
  }};
  Channel::send(line, 7);
  match Channel::poll(line) {{
    Option::Some(value) => print(value),
    Option::None => print(0),
  }};
  Channel::close(line);
}}

fn main() -> Int {{
  look();
  print(khora_live_count());
  0
}}
"
        ),
    );

    assert_eq!(ran.stdout, "0\n7\n0\n", "{}", ran.stdout);
    assert_eq!(ran.code, Some(0));
}
