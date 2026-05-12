const MAX_FUZZY_COMMAND_CHARS: usize = 64;

const COMMANDS: &[&str] = &[
    "!help",
    "!status",
    "!agents",
    "!agent",
    "!sessions",
    "!session",
    "!new",
    "!btw",
    "!gateway",
    "!metrics",
    "!ping",
    "!switch",
    "!default",
    "!model",
    "!secure",
    "!secret",
    "!approve",
    "!deny",
];

pub(super) fn first_arg(text: &str) -> Option<&str> {
    text.split_whitespace().nth(1)
}

pub(super) fn second_arg(text: &str) -> Option<&str> {
    text.split_whitespace().nth(2)
}

pub(super) fn command_token(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

pub(super) fn command_suggestion(cmd: &str) -> Option<&'static str> {
    let raw_without_bang = cmd.trim_start_matches('!');
    if raw_without_bang.chars().count() > MAX_FUZZY_COMMAND_CHARS {
        return None;
    }

    let lower = raw_without_bang.to_lowercase();

    COMMANDS
        .iter()
        .copied()
        .find(|candidate| candidate.trim_start_matches('!') == lower)
        .or_else(|| {
            COMMANDS.iter().copied().find(|candidate| {
                levenshtein_distance(&lower, candidate.trim_start_matches('!')) <= 2
            })
        })
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let b_len = b.chars().count();
    let mut costs: Vec<usize> = (0..=b_len).collect();

    for (i, ca) in a.chars().enumerate() {
        let mut previous = costs[0];
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let insertion = costs[j + 1] + 1;
            let deletion = costs[j] + 1;
            let substitution = previous + usize::from(ca != cb);
            previous = costs[j + 1];
            costs[j + 1] = insertion.min(deletion).min(substitution);
        }
    }

    costs[b_len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_token_ignores_surrounding_whitespace() {
        assert_eq!(command_token("  !agent list  "), "!agent");
        assert_eq!(command_token(""), "");
        assert_eq!(command_token("   "), "");
    }

    #[test]
    fn positional_args_are_whitespace_based() {
        assert_eq!(first_arg("!agent details librarian"), Some("details"));
        assert_eq!(second_arg("!agent details librarian"), Some("librarian"));
        assert_eq!(first_arg("!agent"), None);
        assert_eq!(second_arg("!agent details"), None);
    }

    #[test]
    fn command_suggestions_accept_missing_bang_and_small_typos() {
        assert_eq!(command_suggestion("agents"), Some("!agents"));
        assert_eq!(command_suggestion("!stats"), Some("!status"));
        assert_eq!(command_suggestion("!defualt"), Some("!default"));
    }

    #[test]
    fn command_suggestions_ignore_unbounded_inputs() {
        let long = format!("!{}", "x".repeat(MAX_FUZZY_COMMAND_CHARS + 1));
        assert_eq!(command_suggestion(&long), None);
    }
}
