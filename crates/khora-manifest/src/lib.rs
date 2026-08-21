use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;
use toml::Spanned;

#[derive(Debug)]
pub enum Node {
    Table(Vec<(Spanned<String>, Node)>),
    Leaf,
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any TOML value")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_none<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Leaf)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        Node::deserialize(d)
    }
    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        Node::deserialize(d)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        while seq.next_element::<Node>()?.is_some() {}
        Ok(Node::Leaf)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<Spanned<String>>()? {
            entries.push((key, map.next_value::<Node>()?));
        }
        Ok(Node::Table(entries))
    }
}

pub fn probe() {
    for text in [
        "[a]\nb = 1\n",
        "[a]\nb = 1979-05-27T07:32:00Z\n",
        "when = 1979-05-27\n",
        "[a]\nb = [1, 2]\nc = { d = 1 }\n",
    ] {
        match toml::from_str::<Node>(text) {
            Ok(n) => println!("OK {text:?} -> {n:?}"),
            Err(e) => println!("ERR {text:?} -> {}", e.message()),
        }
    }
}
