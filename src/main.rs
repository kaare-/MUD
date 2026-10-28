mod commands;
mod ui;
mod world;

use bevy::prelude::*;

use ui::{MudState, handle_keyboard, refresh_ui, setup_ui};

fn main() {
    let demo = std::env::args().any(|arg| arg == "--demo");

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "MUD".into(),
            resolution: (960.0_f32, 640.0_f32).into(),
            ..default()
        }),
        ..default()
    }))
    .insert_resource(MudState::new())
    .add_systems(Startup, setup_ui)
    .add_systems(Update, (handle_keyboard, refresh_ui));

    if demo {
        app.insert_resource(DemoScript::default())
            .add_systems(Update, run_demo.after(handle_keyboard));
    }

    app.run();
}

#[derive(Resource)]
struct DemoScript {
    step: usize,
    frames_until_next: u32,
    commands: Vec<&'static str>,
}

impl Default for DemoScript {
    fn default() -> Self {
        Self {
            step: 0,
            frames_until_next: 20,
            commands: vec!["look", "north", "look", "south", "east", "west", "help", "quit"],
        }
    }
}

fn run_demo(
    mut state: ResMut<MudState>,
    mut demo: ResMut<DemoScript>,
    mut exit: EventWriter<AppExit>,
) {
    if demo.frames_until_next > 0 {
        demo.frames_until_next -= 1;
        return;
    }

    if demo.step >= demo.commands.len() {
        exit.send(AppExit::Success);
        return;
    }

    let command = demo.commands[demo.step];
    demo.step += 1;
    demo.frames_until_next = 12;
    info!("demo command: {command}");
    state.input = command.to_string();
    state.submit_input();
}
