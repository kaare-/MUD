use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    North,
    South,
    East,
    West,
}

impl Direction {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "n" | "north" => Some(Self::North),
            "s" | "south" => Some(Self::South),
            "e" | "east" => Some(Self::East),
            "w" | "west" => Some(Self::West),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::North => "north",
            Self::South => "south",
            Self::East => "east",
            Self::West => "west",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Room {
    pub name: &'static str,
    pub description: &'static str,
    pub exits: HashMap<Direction, &'static str>,
}

#[derive(Clone, Debug)]
pub struct World {
    rooms: HashMap<&'static str, Room>,
    pub start_room: &'static str,
}

impl World {
    pub fn starter() -> Self {
        let mut rooms = HashMap::new();

        rooms.insert(
            "entrance",
            Room {
                name: "Entrance Hall",
                description:
                    "Torchlight flickers across worn stone. A carved arch leads deeper into the keep.",
                exits: HashMap::from([
                    (Direction::North, "library"),
                    (Direction::East, "garden"),
                    (Direction::West, "cellar"),
                ]),
            },
        );

        rooms.insert(
            "library",
            Room {
                name: "Dusty Library",
                description:
                    "Shelves lean under the weight of forgotten tomes. Something scuttles between the stacks.",
                exits: HashMap::from([(Direction::South, "entrance")]),
            },
        );

        rooms.insert(
            "garden",
            Room {
                name: "Moonlit Garden",
                description:
                    "Silver grass bends in a quiet wind. A cracked sundial points nowhere useful.",
                exits: HashMap::from([(Direction::West, "entrance")]),
            },
        );

        rooms.insert(
            "cellar",
            Room {
                name: "Damp Cellar",
                description:
                    "The air smells of iron and old wine. Shadow climbs the stair back toward the hall.",
                exits: HashMap::from([(Direction::East, "entrance")]),
            },
        );

        Self {
            rooms,
            start_room: "entrance",
        }
    }

    pub fn room(&self, id: &str) -> Option<&Room> {
        self.rooms.get(id)
    }

    pub fn look(&self, room_id: &str) -> String {
        let Some(room) = self.room(room_id) else {
            return "You are nowhere.".to_string();
        };

        let mut exits: Vec<_> = room.exits.keys().map(|d| d.as_str()).collect();
        exits.sort_unstable();
        let exit_list = if exits.is_empty() {
            "none".to_string()
        } else {
            exits.join(", ")
        };

        format!(
            "{}\n{}\nExits: {}.",
            room.name, room.description, exit_list
        )
    }

    pub fn move_player(
        &self,
        room_id: &str,
        direction: Direction,
    ) -> Result<&'static str, String> {
        let Some(room) = self.room(room_id) else {
            return Err("You are nowhere.".to_string());
        };
        room.exits
            .get(&direction)
            .copied()
            .ok_or_else(|| format!("There is no exit {}.", direction.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_world_links_are_consistent() {
        let world = World::starter();
        let entrance = world.room("entrance").unwrap();
        assert_eq!(entrance.exits.get(&Direction::North), Some(&"library"));
        assert_eq!(
            world.move_player("entrance", Direction::North).unwrap(),
            "library"
        );
        assert!(world.move_player("library", Direction::North).is_err());
    }
}
