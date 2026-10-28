use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;

use crate::commands::{apply_command, help_text, parse_command};
use crate::world::World;

pub const MAX_LOG_LINES: usize = 24;

#[derive(Resource)]
pub struct MudState {
    pub world: World,
    pub room_id: String,
    pub input: String,
    pub log: Vec<String>,
    pub quit_requested: bool,
}

impl MudState {
    pub fn new() -> Self {
        let world = World::starter();
        let room_id = world.start_room.to_string();
        let look = world.look(&room_id);
        let mut log = vec![
            "MUD".to_string(),
            "A torchlit keep waits beyond the threshold.".to_string(),
            help_text().to_string(),
            String::new(),
        ];
        log.extend(look.lines().map(str::to_string));
        Self {
            world,
            room_id,
            input: String::new(),
            log,
            quit_requested: false,
        }
    }

    pub fn push_line(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > MAX_LOG_LINES {
            let overflow = self.log.len() - MAX_LOG_LINES;
            self.log.drain(0..overflow);
        }
    }

    pub fn submit_input(&mut self) {
        let command_text = self.input.trim().to_string();
        self.input.clear();
        if command_text.is_empty() {
            return;
        }

        self.push_line(format!("> {command_text}"));
        let command = parse_command(&command_text);
        let (lines, quit) = apply_command(&self.world, &mut self.room_id, command);
        for line in lines {
            self.push_line(line);
        }
        self.quit_requested = quit;
    }
}

#[derive(Component)]
pub struct LogText;

#[derive(Component)]
pub struct InputText;

#[derive(Component)]
pub struct BrandText;

pub fn setup_ui(mut commands: Commands) {
    commands.spawn(Camera2d::default());

    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(28.0)),
                row_gap: Val::Px(18.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.07, 0.09)),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("MUD"),
                TextFont {
                    font_size: 54.0,
                    ..default()
                },
                TextColor(Color::srgb(0.92, 0.78, 0.42)),
                BrandText,
            ));

            root.spawn((
                Text::new(""),
                TextFont {
                    font_size: 20.0,
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.84, 0.80)),
                Node {
                    flex_grow: 1.0,
                    ..default()
                },
                LogText,
            ));

            root.spawn((
                Text::new("> "),
                TextFont {
                    font_size: 22.0,
                    ..default()
                },
                TextColor(Color::srgb(0.95, 0.93, 0.86)),
                InputText,
            ));
        });
}

pub fn refresh_ui(
    state: Res<MudState>,
    mut log_query: Query<&mut Text, (With<LogText>, Without<InputText>)>,
    mut input_query: Query<&mut Text, (With<InputText>, Without<LogText>)>,
) {
    if let Ok(mut text) = log_query.get_single_mut() {
        *text = Text::new(state.log.join("\n"));
    }
    if let Ok(mut text) = input_query.get_single_mut() {
        *text = Text::new(format!("> {}_", state.input));
    }
}

pub fn handle_keyboard(
    mut state: ResMut<MudState>,
    mut events: EventReader<KeyboardInput>,
    mut exit: EventWriter<AppExit>,
) {
    for event in events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }

        match &event.logical_key {
            Key::Enter => state.submit_input(),
            Key::Backspace => {
                state.input.pop();
            }
            Key::Character(chars) => {
                for ch in chars.chars() {
                    if !ch.is_control() {
                        state.input.push(ch);
                    }
                }
            }
            _ => {}
        }
    }

    if state.quit_requested {
        exit.send(AppExit::Success);
    }
}
