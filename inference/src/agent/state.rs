//! A chat saved to resume: the backend's exact context and the agent's state, as JSON
//! (`<transcript>.state.json`, rewritten after each round). `--resume` reads it back, so
//! a chat survives a crash, a power cut or a restart, a paused turn included.

use serde_json::{json, Value};

use super::flow::Round;
use super::{Call, Exchange, Pending};

/// The snapshot format; 1 was the chat before the shared loop (kimi `chat.rs`).
pub const VERSION: u64 = 2;

pub fn call_to_json(c: &Call) -> Value {
    json!({"id": c.id, "name": c.name, "arguments": c.arguments})
}

pub fn call_from_json(v: &Value) -> Result<Call, String> {
    Ok(Call {
        id: v["id"].as_str().map(str::to_owned),
        name: v["name"]
            .as_str()
            .ok_or("a saved call has no name")?
            .to_owned(),
        arguments: v["arguments"].as_str().unwrap_or("{}").to_owned(),
    })
}

fn results_to_json(results: &[(Call, String)]) -> Value {
    results
        .iter()
        .map(|(c, r)| json!({"call": call_to_json(c), "result": r}))
        .collect()
}

fn results_from_json(v: &Value) -> Result<Vec<(Call, String)>, String> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            Ok((
                call_from_json(&r["call"])?,
                r["result"].as_str().unwrap_or_default().to_owned(),
            ))
        })
        .collect()
}

pub fn tokens_from_json(v: &Value) -> Result<Vec<u32>, String> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            t.as_u64()
                .and_then(|t| u32::try_from(t).ok())
                .ok_or_else(|| "a saved token is not a token id".to_owned())
        })
        .collect()
}

pub fn round_to_json(r: &Round) -> Value {
    json!({"reply": r.reply, "ended_by": r.ended_by, "results": results_to_json(&r.results)})
}

pub fn round_from_json(v: &Value) -> Result<Round, String> {
    Ok(Round {
        reply: tokens_from_json(&v["reply"])?,
        ended_by: v["ended_by"].as_u64().and_then(|t| u32::try_from(t).ok()),
        results: results_from_json(&v["results"])?,
    })
}

pub fn pending_to_json(p: &Pending) -> Value {
    match p {
        Pending::Calls { done, remaining } => json!({
            "kind": "calls",
            "done": results_to_json(done),
            "remaining": remaining.iter().map(call_to_json).collect::<Vec<_>>(),
        }),
        Pending::Reply(tokens) => json!({"kind": "reply", "tokens": tokens}),
    }
}

pub fn pending_from_json(v: &Value) -> Result<Option<Pending>, String> {
    match v["kind"].as_str() {
        None => Ok(None),
        Some("calls") => Ok(Some(Pending::Calls {
            done: results_from_json(&v["done"])?,
            remaining: v["remaining"]
                .as_array()
                .into_iter()
                .flatten()
                .map(call_from_json)
                .collect::<Result<_, _>>()?,
        })),
        Some("reply") => Ok(Some(Pending::Reply(tokens_from_json(&v["tokens"])?))),
        Some(other) => Err(format!("unknown paused-turn kind {other:?}")),
    }
}

pub fn history_to_json(h: &[Exchange]) -> Value {
    h.iter()
        .map(|e| json!({"user": e.user, "answer": e.answer}))
        .collect()
}

pub fn history_from_json(v: &Value) -> Vec<Exchange> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|e| Exchange {
            user: e["user"].as_str().unwrap_or_default().to_owned(),
            answer: e["answer"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

pub fn usize_list(v: &Value) -> Vec<usize> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_u64().and_then(|n| usize::try_from(n).ok()))
        .collect()
}
