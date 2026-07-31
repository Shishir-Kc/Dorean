//! `@agentname` mention routing between the user and sub-agents, and between
//! agents. Agent names feed the TUI autocomplete (Phase 6).

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;

use crate::error::DoreanError;

use super::manifest::AgentManifest;

/// Extract `@name` mentions from text, restricted to known agents. Names are
/// matched as whole tokens (`@backend`, not `@backenden`).
pub fn parse_mentions(text: &str, known: &HashSet<&str>) -> Vec<String> {
    let mut mentions = Vec::new();
    for token in text.split_whitespace() {
        if !token.starts_with('@') {
            continue;
        }
        let raw = &token[1..];
        let name = raw.trim_end_matches(|c: char| c.is_ascii_punctuation());
        if !name.is_empty() && known.contains(name) {
            mentions.push(name.to_string());
        }
    }
    // Preserve first-mention order, drop duplicates.
    let mut seen = HashSet::new();
    mentions
        .into_iter()
        .filter(|m| seen.insert(m.clone()))
        .collect()
}

/// One routed message: `from` agent (or `user`) → `to` agent, with text.
#[derive(Debug, Clone)]
pub struct Mention {
    pub from: String,
    pub to: String,
    pub text: String,
}

/// Fan-out router. The orchestrator holds the senders; each sub-agent is given
/// its own [`mpsc::UnboundedReceiver`] to drain inbound mentions at the start
/// of each work round.
#[derive(Debug, Clone)]
pub struct Router {
    senders: HashMap<String, mpsc::UnboundedSender<Mention>>,
}

impl Router {
    /// Build a router with a channel per agent.
    pub fn new(
        agents: &[AgentManifest],
    ) -> (Router, HashMap<String, mpsc::UnboundedReceiver<Mention>>) {
        let mut senders = HashMap::new();
        let mut receivers = HashMap::new();
        for agent in agents {
            let (tx, rx) = mpsc::unbounded_channel();
            senders.insert(agent.name.clone(), tx);
            receivers.insert(agent.name.clone(), rx);
        }
        (Router { senders }, receivers)
    }

    /// Known agent names, sorted (feeds autocomplete).
    pub fn agent_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.senders.keys().cloned().collect();
        names.sort();
        names
    }

    /// Route a mention. Fails if the target is not a known agent.
    pub fn route(&self, from: &str, to: &str, text: &str) -> Result<(), DoreanError> {
        let Some(tx) = self.senders.get(to) else {
            return Err(DoreanError::Message(format!(
                "route: unknown agent `@{to}` (known: {})",
                self.agent_names().join(", ")
            )));
        };
        let mention = Mention {
            from: from.to_string(),
            to: to.to_string(),
            text: text.to_string(),
        };
        tx.send(mention)
            .map_err(|_| DoreanError::Message(format!("route: `@{to}` is not listening")))?;
        Ok(())
    }
}

/// A router plus its receiver map, ready to hand to a parallel run. Built by
/// the TUI so the user can route `@agent` mentions during a live run.
pub struct Routing {
    pub router: Router,
    pub receivers: HashMap<String, mpsc::UnboundedReceiver<Mention>>,
}

impl Routing {
    pub fn new(agents: &[AgentManifest]) -> Self {
        let (router, receivers) = Router::new(agents);
        Routing { router, receivers }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents(names: &[&str]) -> Vec<AgentManifest> {
        names
            .iter()
            .map(|n| AgentManifest {
                name: n.to_string(),
                ..AgentManifest::default()
            })
            .collect()
    }

    fn known<'a>(names: &[&'a str]) -> HashSet<&'a str> {
        names.iter().copied().collect()
    }

    #[test]
    fn parses_known_mentions_only() {
        let names = known(&["backend", "db"]);
        assert_eq!(
            parse_mentions("@backend fix the API, then @db migrate", &names),
            vec!["backend".to_string(), "db".to_string()]
        );
        // Unknown names and punctuation are ignored.
        assert!(
            parse_mentions("@nobody and @backend!", &names)
                .iter()
                .all(|m| m == "backend")
        );
        // Prefix must not match (whole-token).
        assert!(parse_mentions("@backenden todo", &names).is_empty());
    }

    #[test]
    fn dedupes_mentions() {
        let names = known(&["backend"]);
        assert_eq!(
            parse_mentions("@backend do it @backend again", &names),
            vec!["backend".to_string()]
        );
    }

    #[test]
    fn routes_between_agents() {
        let (router, mut receivers) = Router::new(&agents(&["backend", "db"]));
        router
            .route("db", "backend", "@backend wire it up")
            .unwrap();
        let rx = receivers.get_mut("backend").unwrap();
        let mention = rx.try_recv().unwrap();
        assert_eq!(mention.from, "db");
        assert_eq!(mention.to, "backend");
        assert!(mention.text.contains("wire it up"));
    }

    #[test]
    fn route_unknown_agent_fails() {
        let (router, _) = Router::new(&agents(&["backend"]));
        assert!(router.route("user", "nope", "hi").is_err());
    }

    #[test]
    fn agent_names_are_sorted() {
        let (router, _) = Router::new(&agents(&["db", "backend"]));
        assert_eq!(
            router.agent_names(),
            vec!["backend".to_string(), "db".to_string()]
        );
    }
}
