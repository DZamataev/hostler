//! `hostler guide` (plan Task 12): plain text by default, the contract's JSON envelope with
//! `--json`.

use std::process::ExitCode;

use serde_json::json;

use crate::contract::{Envelope, ErrorCode, HostlerError};
use crate::guide::{self, TopicSummary, TopicText};

pub fn main(topic: Option<String>, json: bool) -> ExitCode {
    let Some(topic) = topic else {
        let topics: Vec<TopicSummary> = guide::TOPICS
            .iter()
            .map(|(name, summary)| TopicSummary { name, summary })
            .collect();
        if json {
            print_json(Envelope {
                ok: true,
                data: Some(topics),
                error: None,
            });
        } else {
            println!("hostler guide — topics:");
            for topic in &topics {
                println!("  {:<8} {}", topic.name, topic.summary);
            }
            println!("\nRun `hostler guide <topic>` for the full text.");
        }
        return ExitCode::SUCCESS;
    };

    match guide::topic(&topic) {
        Some(text) => {
            if json {
                print_json(Envelope {
                    ok: true,
                    data: Some(TopicText { name: topic, text }),
                    error: None,
                });
            } else {
                print!("{text}");
            }
            ExitCode::SUCCESS
        }
        None => {
            let names = guide::topic_names();
            let error = HostlerError {
                code: ErrorCode::BadRequest,
                message: format!("no guide topic {topic:?}; topics: {}", names.join(", ")),
                details: json!({ "topics": names, "requested": topic }),
                recovery: Some("hostler guide".into()),
            };
            if json {
                print_json(Envelope::<()> {
                    ok: false,
                    data: None,
                    error: Some(error),
                });
            } else {
                eprintln!("[hostler] {}", error.message);
                if let Some(recovery) = &error.recovery {
                    eprintln!("[hostler] To fix: {recovery}");
                }
            }
            ExitCode::FAILURE
        }
    }
}

fn print_json(envelope: Envelope<impl serde::Serialize>) {
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("a guide envelope always encodes")
    );
}
