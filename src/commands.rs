use crate::world::{Direction, World};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Look,
    Go(Direction),
    Help,
    Quit,
    Unknown(String),
}

pub fn parse_command(input: &str) -> Command {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Command::Unknown(String::new());
    }

    let mut parts = trimmed.split_whitespace();
    let verb = parts.next().unwrap_or_default().to_ascii_lowercase();
    let rest: Vec<_> = parts.collect();

    match verb.as_str() {
        "l" | "look" => Command::Look,
        "help" | "?" => Command::Help,
        "quit" | "exit" | "q" => Command::Quit,
        "go" | "move" => {
            if let Some(dir) = rest.first().and_then(|d| Direction::parse(d)) {
                Command::Go(dir)
            } else {
                Command::Unknown(trimmed.to_string())
            }
        }
        other => {
            if let Some(dir) = Direction::parse(other) {
                Command::Go(dir)
            } else {
                Command::Unknown(trimmed.to_string())
            }
        }
    }
}

pub fn help_text() -> &'static str {
    "Commands: look (l), go <dir>, north/south/east/west (n/s/e/w), help, quit"
}

pub fn apply_command(
    world: &World,
    room_id: &mut String,
    command: Command,
) -> (Vec<String>, bool) {
    match command {
        Command::Look => (vec![world.look(room_id)], false),
        Command::Go(direction) => match world.move_player(room_id, direction) {
            Ok(next) => {
                *room_id = next.to_string();
                let mut lines = vec![format!("You walk {}.", direction.as_str())];
                lines.push(world.look(room_id));
                (lines, false)
            }
            Err(message) => (vec![message], false),
        },
        Command::Help => (vec![help_text().to_string()], false),
        Command::Quit => (vec!["Farewell, traveler.".to_string()], true),
        Command::Unknown(raw) if raw.is_empty() => (Vec::new(), false),
        Command::Unknown(raw) => (
            vec![format!("Unknown command: {raw}. Type 'help'.")],
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cardinal_shortcuts() {
        assert_eq!(parse_command("n"), Command::Go(Direction::North));
        assert_eq!(parse_command("go west"), Command::Go(Direction::West));
        assert_eq!(parse_command("look"), Command::Look);
    }

    #[test]
    fn moving_updates_room() {
        let world = World::starter();
        let mut room = world.start_room.to_string();
        let (lines, quit) = apply_command(&world, &mut room, Command::Go(Direction::East));
        assert!(!quit);
        assert_eq!(room, "garden");
        assert!(lines[0].contains("east"));
        assert!(lines[1].contains("Moonlit Garden"));
    }
}
