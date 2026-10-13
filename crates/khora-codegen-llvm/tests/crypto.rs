#![cfg(feature = "llvm")]

//! `std::crypto`, against the real `std` and against published vectors.
//!
//! **What these exist to catch: a binding that answers plausibly and wrongly.**
//! A digest that is off by one byte of input, an HMAC whose key and message
//! were swapped, a base64 that pads the URL-safe alphabet: every one of those
//! produces output that looks exactly like a digest. Only a known answer tells
//! them apart, so every expected value here is a published one (FIPS 180-4,
//! RFC 4231, RFC 4648) or Python's `hashlib`/`hmac`, which is OpenSSL and so a
//! different implementation from the `ring` this binds.

use crate::harness;

use std::path::PathBuf;
use std::process::Command;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every `.kh` file of `std` for the host, plus the program under test.
fn sources(db: &KhoraDatabase, dir: &std::path::Path, main: &str) -> Vec<SourceFile> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                out.push(SourceFile::new(db, path, text));
            }
        }
    }
    out.push(SourceFile::new(db, dir.join("main.kh"), main.to_string()));
    out
}

fn compiled(name: &str, main: &str) -> Result<PathBuf, Vec<String>> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let root = SourceRoot::new(&db, sources(&db, &dir, main));
    match khora_codegen_llvm::compile(&db, root, &exe) {
        Ok(_) => Ok(exe),
        Err(errors) => Err(errors.into_iter().map(|e| e.message).collect()),
    }
}

fn build(name: &str, main: &str) -> PathBuf {
    compiled(name, main)
        .unwrap_or_else(|messages| panic!("compiling `{name}` failed:\n  {}\n\n{main}", messages.join("\n  ")))
}

fn run(name: &str, main: &str) -> String {
    let exe = build(name, main);
    let output = Command::new(&exe).output().expect("the program should run");
    assert!(
        output.status.success(),
        "`{name}` exited with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// The program's lines, checked one by one against `expected`, so a failure
/// names the vector rather than printing two walls of hex.
fn assert_lines(out: &str, expected: &[&str]) {
    let lines: Vec<&str> = out.trim_end().lines().collect();
    for (at, want) in expected.iter().enumerate() {
        assert_eq!(lines.get(at).copied(), Some(*want), "line {at} of:\n{out}");
    }
    assert_eq!(lines.len(), expected.len(), "line count of:\n{out}");
}

// --- digests ----------------------------------------------------------------

/// FIPS 180-4's examples: the empty message, `abc`, and the two-block messages
/// (56 bytes for SHA-256, 112 for SHA-512), which are the ones that exercise
/// padding across a block boundary.
#[test]
fn sha2_matches_fips_180_4() {
    let out = run(
        "crypto_sha2",
        r#"module main;
import std::core::{Array, print};
import std::crypto::{Hex, sha256, sha512};

fn both(text: String) -> () {
  print(Hex::encode(sha256(String::bytes(text))));
  print(Hex::encode(sha512(String::bytes(text))));
}

pub fn main() -> () {
  both("");
  both("abc");
  both("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
  both("abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu");
}
"#,
    );
    assert_lines(
        &out,
        &[
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445",
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909",
        ],
    );
}

// --- HMAC -------------------------------------------------------------------

/// RFC 4231 test cases 1, 2, 4 and 6, both widths.
///
/// Case 6's key is 131 bytes, longer than either hash's block, which is the
/// case where the key is hashed first; a binding that skipped that step would
/// pass the other three. Keys and messages arrive as hex so the vectors are
/// the RFC's own text.
#[test]
fn hmac_matches_rfc_4231() {
    let out = run(
        "crypto_hmac",
        r#"module main;
import std::core::{Array, Option, Redacted, print};
import std::crypto::{Hex, Hmac};

fn bytes(hex: String) -> Array<U8> {
  match Hex::decode(hex) {
    Option::Some(b) => b,
    Option::None => String::bytes("not hex"),
  }
}

fn case(key: String, message: String) -> () {
  let k = Redacted::of(bytes(key));
  print(Hex::encode(Hmac::sha256(k, bytes(message))));
  print(Hex::encode(Hmac::sha512(k, bytes(message))));
}

pub fn main() -> () {
  case("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b", "4869205468657265");
  case("4a656665", "7768617420646f2079612077616e7420666f72206e6f7468696e673f");
  case("0102030405060708090a0b0c0d0e0f10111213141516171819",
       "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd");
  let long_key = Hex::encode(Array::new(131, U8::of(170)));
  case(long_key,
       "54657374205573696e67204c6172676572205468616e20426c6f636b2d53697a65204b6579202d2048617368204b6579204669727374");
}
"#,
    );
    assert_lines(
        &out,
        &[
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737",
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            "b0ba465637458c6990e5a8c5f61d4af7e576d97ff94b872de76f8050361ee3dba91ca5c11aa25eb4d679275cc5788063a5f19741120c4f2de2adebeb10a298dd",
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f3526b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598",
        ],
    );
}

/// `verify` accepts the tag `sign` made and refuses every near miss: one bit
/// flipped (in the first byte and in the last, because a comparison that stops
/// early differs at one end and not the other), a truncated tag, an empty tag,
/// and the right tag under the wrong key. Both widths.
#[test]
fn verify_accepts_the_tag_and_refuses_near_misses() {
    let out = run(
        "crypto_verify",
        r#"module main;
import std::core::{Array, Redacted, print};
import std::crypto::{Hmac, constant_time_eq};

fn flipped(tag: Array<U8>, at: Int) -> Array<U8> {
  let copy = Array::prefix(tag, Array::length(tag));
  Array::set(copy, at, U8::xor(Array::get(copy, at), U8::of(1)));
  copy
}

fn say(label: String, answer: Bool) -> () {
  print("${label} ${if answer { "accepted" } else { "refused" }}");
}

pub fn main() -> () {
  let key = Redacted::of(String::bytes("a webhook secret"));
  let other = Redacted::of(String::bytes("a webhook secreT"));
  let message = String::bytes("{\"event\":\"paid\"}");

  let tag = Hmac::sha256(key, message);
  let last = Array::length(tag) - 1;
  say("256 right", Hmac::verify_sha256(key, message, tag));
  say("256 first-bit", Hmac::verify_sha256(key, message, flipped(tag, 0)));
  say("256 last-bit", Hmac::verify_sha256(key, message, flipped(tag, last)));
  say("256 truncated", Hmac::verify_sha256(key, message, Array::prefix(tag, last)));
  say("256 empty", Hmac::verify_sha256(key, message, Array::empty()));
  say("256 wrong-key", Hmac::verify_sha256(other, message, tag));

  let long = Hmac::sha512(key, message);
  let end = Array::length(long) - 1;
  say("512 right", Hmac::verify_sha512(key, message, long));
  say("512 first-bit", Hmac::verify_sha512(key, message, flipped(long, 0)));
  say("512 last-bit", Hmac::verify_sha512(key, message, flipped(long, end)));
  say("512 truncated", Hmac::verify_sha512(key, message, Array::prefix(long, end)));
  say("512 empty", Hmac::verify_sha512(key, message, Array::empty()));
  say("512 as-256", Hmac::verify_sha512(key, message, tag));

  say("eq same", constant_time_eq(tag, Array::prefix(tag, Array::length(tag))));
  say("eq bit", constant_time_eq(tag, flipped(tag, last)));
  say("eq shorter", constant_time_eq(tag, Array::prefix(tag, last)));
  say("eq empty", constant_time_eq(Array::empty(), Array::empty()));
}
"#,
    );
    assert_lines(
        &out,
        &[
            "256 right accepted",
            "256 first-bit refused",
            "256 last-bit refused",
            "256 truncated refused",
            "256 empty refused",
            "256 wrong-key refused",
            "512 right accepted",
            "512 first-bit refused",
            "512 last-bit refused",
            "512 truncated refused",
            "512 empty refused",
            "512 as-256 refused",
            "eq same accepted",
            "eq bit refused",
            "eq shorter refused",
            "eq empty accepted",
        ],
    );
}

// --- the key stays a secret -------------------------------------------------

/// **A key shows as `<redacted>`, alone and inside a record**, and the bytes
/// never reach the output in any spelling: not as text, and not as the hex a
/// person would paste into a log.
#[test]
fn a_key_shows_as_redacted() {
    let out = run(
        "crypto_key_show",
        r#"module main;
import std::core::{Array, Redacted, Show, print};
import std::crypto::{Hex, Hmac};

derive(Show)
type Signer = { name: String, key: Redacted<Array<U8>> };

pub fn main() -> () {
  let key = Redacted::of(String::bytes("hunter2-hunter2"));
  print(key.show());
  print("${key}");
  let signer: Signer = { name: "billing", key: key };
  print(signer.show());
  print(Hex::encode(Hmac::sha256(key, String::bytes("x"))));
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "<redacted>");
    assert_eq!(lines[1], "<redacted>");
    assert_eq!(lines[2], "Signer { name: billing, key: <redacted> }");
    assert!(!out.contains("hunter2"), "the key reached stdout as text: {out}");
    // `hunter2-hunter2` in hex, which is how a byte key would leak.
    assert!(!out.contains("68756e74657232"), "the key reached stdout as hex: {out}");
}

/// **Encoding a key does not compile.** `Redacted` has no `Encode`, so a
/// record carrying a signing key cannot be put in a response body or a
/// structured log line by accident.
#[test]
fn encoding_a_key_does_not_compile() {
    let Err(messages) = compiled(
        "crypto_key_encode",
        r#"module main;
import std::core::{Array, Redacted, print};
import std::schema::{Encode};

derive(Encode)
type Leak = { key: Redacted<Array<U8>> };

pub fn main() -> () { print("unreachable"); }
"#,
    ) else {
        panic!("a record holding a key derived `Encode`, and it should not have");
    };
    assert!(
        messages.iter().any(|m| m.contains("derive(Encode)") && m.contains("Redacted")),
        "expected the derive to name the field it could not write, got {messages:?}"
    );
}

/// **No `std::crypto` function hands a key back.**
///
/// The functions that take a key answer a tag or a `Bool`, and the module's
/// only way to the bytes, `Redacted::expose`, must appear nowhere but as the
/// array lent straight to the runtime. A new function that bound the exposed
/// key to a name, returned it or put it in a record would fail here, because
/// that is the step a review of a diff most easily misses.
#[test]
fn no_crypto_function_returns_the_key() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join("crypto_native.kh");
    let text = std::fs::read_to_string(&path).expect("std::crypto's source");
    let code: Vec<&str> = text
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect();
    let exposes: Vec<&str> = code.iter().copied().filter(|l| l.contains("expose")).collect();
    assert!(!exposes.is_empty(), "std::crypto takes keys and never reads one");
    for line in exposes {
        assert!(
            line.contains("Array::with_data(Redacted::expose("),
            "a key is exposed for something other than lending it to the runtime: {line}"
        );
    }
    // And every public function taking a key answers a tag, a decision, or a
    // derived key that is itself redacted: never bytes the key, or something
    // that stands in for it, could be read back out of. A PBKDF2 output is
    // password-equivalent in SCRAM, so it may not come back as a bare array.
    for line in code.iter().filter(|l| l.contains("pub fn") && l.contains("Redacted")) {
        let answer = line.rsplit("->").next().unwrap_or("").trim();
        assert!(
            answer.starts_with("Array<U8>")
                || answer.starts_with("Option<Redacted<Array<U8>>>")
                || answer.starts_with("Bool"),
            "a function taking a key answers something other than a tag or a decision: {line}"
        );
        assert!(
            !line.contains("pbkdf2") || answer.starts_with("Option<Redacted<"),
            "a derived key comes back unredacted: {line}"
        );
    }
}

// --- encodings --------------------------------------------------------------

/// RFC 4648 §10, both alphabets, and hex, each decoded back.
#[test]
fn encodings_match_rfc_4648_and_round_trip() {
    let out = run(
        "crypto_encodings",
        r#"module main;
import std::core::{Array, Option, print};
import std::crypto::{Base64, Hex};

fn back(decoded: Option<Array<U8>>) -> String {
  match decoded {
    Option::Some(b) => String::from_bytes(b),
    Option::None => "<refused>",
  }
}

fn row(text: String) -> () {
  let b = String::bytes(text);
  let padded = Base64::encode(b);
  let url = Base64::encode_url_safe(b);
  let hex = Hex::encode(b);
  print("${padded} ${url} ${hex} ${back(Base64::decode(padded))}|${back(Base64::decode_url_safe(url))}|${back(Hex::decode(hex))}|");
}

pub fn main() -> () {
  row(""); row("f"); row("fo"); row("foo"); row("foob"); row("fooba"); row("foobar");
  let odd: Array<U8> = Array::new(3, U8::of(0));
  Array::set(odd, 0, U8::of(251)); Array::set(odd, 1, U8::of(255)); Array::set(odd, 2, U8::of(191));
  print("${Base64::encode(odd)} ${Base64::encode_url_safe(odd)} ${Hex::encode(odd)}");
  print(back(Hex::decode("666F6f")));
  print(back(Hex::decode("abc")));
  print(back(Hex::decode("zz")));
  print(back(Base64::decode("Zg")));
  print(back(Base64::decode("Zh==")));
  print(back(Base64::decode_url_safe("Zg==")));
  print(back(Base64::decode("Zm9v YmFy")));
}
"#,
    );
    assert_lines(
        &out,
        &[
            "   |||",
            "Zg== Zg 66 f|f|f|",
            "Zm8= Zm8 666f fo|fo|fo|",
            "Zm9v Zm9v 666f6f foo|foo|foo|",
            "Zm9vYg== Zm9vYg 666f6f62 foob|foob|foob|",
            "Zm9vYmE= Zm9vYmE 666f6f6261 fooba|fooba|fooba|",
            "Zm9vYmFy Zm9vYmFy 666f6f626172 foobar|foobar|foobar|",
            "+/+/ -_-_ fbffbf",
            "foo",
            "<refused>",
            "<refused>",
            "<refused>",
            "<refused>",
            "<refused>",
            "<refused>",
        ],
    );
}

// --- secure random ----------------------------------------------------------

/// `SecureRandom::real()` fills the whole buffer, two draws differ, and a
/// handler a test writes is the one a function that takes the capability uses.
#[test]
fn secure_random_fills_differs_and_can_be_replaced() {
    let out = run(
        "crypto_secure_random",
        r#"module main;
import std::core::{Array, print};
import std::crypto::{Hex, SecureRandom};

fn secret() -> String with { random: SecureRandom } {
  let buffer: Array<U8> = Array::new(32, U8::of(0));
  random.bytes(buffer);
  Hex::encode(buffer)
}

fn sevens(buffer: Array<U8>) -> () {
  let mut at = 0;
  while at < Array::length(buffer) {
    Array::set(buffer, at, U8::of(7));
    at = at + 1;
  }
}

pub fn main() -> () {
  let first = with { random: SecureRandom::real() } { secret() };
  let second = with { random: SecureRandom::real() } { secret() };
  print(first);
  print(second);
  let pinned = handler for SecureRandom {
    bytes: fn buffer => sevens(buffer),
  };
  print(with { random: pinned } { secret() });
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[0].len(), 64, "thirty-two bytes of hex: {out}");
    // An all-zero buffer means nothing wrote to it; a real source gives that
    // with probability 2^-256.
    assert_ne!(lines[0], "0".repeat(64), "the buffer was not filled");
    assert_ne!(lines[0], lines[1], "two draws from the operating system were equal");
    assert_eq!(lines[2], "07".repeat(32), "the test's handler was not the one used");
}

// --- end to end -------------------------------------------------------------

/// **A webhook signed in Khora verifies in Python**, which is the claim a
/// reader of the cookbook is relying on: that the header this program sets is
/// the one GitHub's or Stripe's documentation tells the receiving side to
/// compute. Python's `hmac` is OpenSSL, not `ring`, so the two agreeing is
/// agreement between independent implementations.
#[test]
fn a_signed_webhook_verifies_in_python() {
    let out = run(
        "crypto_webhook",
        r#"module main;
import std::core::{Array, Redacted, print};
import std::crypto::{Hex, Hmac};

pub fn main() -> () {
  let secret = Redacted::of(String::bytes("whsec_5WbX5kEWLlfzsGNjH64I8lOOqUB6e8FH"));
  let payload = "{\"id\":\"evt_1\",\"amount\":4200,\"currency\":\"usd\"}";
  print(payload);
  print("sha256=${Hex::encode(Hmac::sha256(secret, String::bytes(payload)))}");
}
"#,
    );
    let mut lines = out.lines();
    let payload = lines.next().expect("the payload");
    let header = lines.next().expect("the header");
    let check = Command::new(harness::python())
        .arg("-c")
        .arg(
            "import hmac, hashlib, sys\n\
             want = 'sha256=' + hmac.new(b'whsec_5WbX5kEWLlfzsGNjH64I8lOOqUB6e8FH', \
             sys.argv[1].encode(), hashlib.sha256).hexdigest()\n\
             print('agree' if hmac.compare_digest(want, sys.argv[2]) else 'differ ' + want)\n",
        )
        .arg(payload)
        .arg(header)
        .output()
        .expect("python should run");
    let said = String::from_utf8_lossy(&check.stdout);
    assert_eq!(said.trim(), "agree", "python: {said} {}", String::from_utf8_lossy(&check.stderr));
}
