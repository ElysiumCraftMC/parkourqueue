use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;
use rand::seq::SliceRandom;
use valence::prelude::*;
use valence::protocol::sound::{Sound, SoundCategory};
use valence::scoreboard::*;
use valence::spawn::IsFlat;
use valence::title::SetTitle;
use valence::{CompressionThreshold, ServerSettings};

const START_POS: BlockPos = BlockPos::new(0, 100, 0);
const VIEW_DIST: u8 = 10;

const BLOCK_TYPES: [BlockState; 1] = [BlockState::OBSIDIAN];

pub fn main() {
    let velocity_secret = std::env::var("VELOCITY_SECRET").expect("Expected VELOCITY_SECRET env");
    let secret_arc = Arc::from(velocity_secret);
    let address = std::env::var("ADDRESS").expect("Expected ADDRESS env");
    let address: SocketAddr = address.parse().expect("Failed to parse ADDRESS");

    App::new()
        .insert_resource(ServerSettings {
            compression_threshold: CompressionThreshold(-1),
            ..Default::default()
        })
        .insert_resource(NetworkSettings {
            connection_mode: ConnectionMode::Velocity { secret: secret_arc },
            max_players: i32::MAX as usize,
            address,
            ..Default::default()
        })
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                init_clients,
                reset_clients.after(init_clients),
                manage_chunks.after(reset_clients).before(manage_blocks),
                manage_blocks,
                despawn_disconnected_clients,
            ),
        )
        .run();
}

#[derive(Debug, Resource)]
struct Globals {
    pub scoreboard_layer: Entity,
}

#[derive(Component)]
struct GameState {
    blocks: VecDeque<BlockPos>,
    score: u32,
    combo: u32,
    target_y: i32,
    last_block_timestamp: u128,
}

fn setup(
    mut commands: Commands,
    server: Res<Server>,
    dimensions: Res<DimensionTypeRegistry>,
    biomes: Res<BiomeRegistry>,
) {
    let parkour_objective_layer = commands.spawn(EntityLayer::new(&server)).id();
    let parkour_objective = ObjectiveBundle {
        name: Objective::new("parkour-jumps"),
        display: ObjectiveDisplay("Best scores".into_text()),
        layer: EntityLayerId(parkour_objective_layer),
        ..Default::default()
    };
    commands.spawn(parkour_objective);

    let globals = Globals {
        scoreboard_layer: parkour_objective_layer,
    };
    commands.insert_resource(globals);
}

fn init_clients(
    mut clients: Query<
        (
            Entity,
            &mut Client,
            &mut VisibleChunkLayer,
            &mut VisibleEntityLayers,
            &mut IsFlat,
            &mut GameMode,
        ),
        Added<Client>,
    >,
    server: Res<Server>,
    dimensions: Res<DimensionTypeRegistry>,
    biomes: Res<BiomeRegistry>,
    mut commands: Commands,
    globals: Res<Globals>,
) {
    for (
        entity,
        mut client,
        mut visible_chunk_layer,
        mut visible_entity_layers,
        mut is_flat,
        mut game_mode,
    ) in &mut clients
    {
        visible_chunk_layer.0 = entity;
        visible_entity_layers.0.insert(entity);
        visible_entity_layers.0.insert(globals.scoreboard_layer);
        is_flat.0 = true;
        *game_mode = GameMode::Adventure;

        let state = GameState {
            blocks: VecDeque::new(),
            score: 0,
            combo: 0,
            target_y: 0,
            last_block_timestamp: 0,
        };

        let layer = ChunkLayer::new(ident!("the_end"), &dimensions, &biomes, &server);

        commands.entity(entity).insert((state, layer));
    }
}

fn reset_clients(
    mut clients: Query<(
        &mut Client,
        &mut Position,
        &mut Look,
        &mut GameState,
        &mut ChunkLayer,
    )>,
) {
    for (mut client, mut pos, mut look, mut state, mut layer) in &mut clients {
        let out_of_bounds = (pos.0.y as i32) < START_POS.y - 32;

        if out_of_bounds || state.is_added() {
            if out_of_bounds && !state.is_added() {
                client.send_chat_message(
                    "Your score was ".italic()
                        + state
                            .score
                            .to_string()
                            .color(Color::GOLD)
                            .bold()
                            .not_italic(),
                );
            }

            for pos in ChunkView::new(START_POS.into(), VIEW_DIST).iter() {
                layer.insert_chunk(pos, UnloadedChunk::new());
            }

            state.score = 0;
            state.combo = 0;

            for block in &state.blocks {
                layer.set_block(*block, BlockState::AIR);
            }
            state.blocks.clear();
            state.blocks.push_back(START_POS);
            layer.set_block(START_POS, BlockState::BLACK_WOOL);

            for _ in 0..10 {
                generate_next_block(&mut state, &mut layer, false);
            }

            pos.set([
                f64::from(START_POS.x) + 0.5,
                f64::from(START_POS.y) + 1.0,
                f64::from(START_POS.z) + 0.5,
            ]);
            look.yaw = 0.0;
            look.pitch = 0.0;
        }
    }
}

fn manage_blocks(
    mut clients: Query<(
        &mut Client,
        &Position,
        &mut GameState,
        &mut ChunkLayer,
        &Username,
    )>,
    mut objectives: Query<&mut ObjectiveScores, With<Objective>>,
) {
    for (mut client, pos, mut state, mut layer, username) in &mut clients {
        let pos_under_player = BlockPos::new(
            (pos.0.x - 0.5).round() as i32,
            pos.0.y as i32 - 1,
            (pos.0.z - 0.5).round() as i32,
        );

        if let Some(index) = state
            .blocks
            .iter()
            .position(|block| *block == pos_under_player)
        {
            if index > 0 {
                let power_result = 2_f32.powf((state.combo as f32) / 45.0);
                let max_time_taken = (1000_f32 * (index as f32) / power_result) as u128;

                let current_time_millis = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis();

                if current_time_millis - state.last_block_timestamp < max_time_taken {
                    state.combo += index as u32
                } else {
                    state.combo = 0
                }

                for _ in 0..index {
                    generate_next_block(&mut state, &mut layer, true)
                }

                let pitch = 0.9 + ((state.combo as f32) - 1.0) * 0.05;
                client.play_sound(
                    Sound::BlockNoteBlockBass,
                    SoundCategory::Master,
                    pos.0,
                    1.0,
                    pitch,
                );

                client.set_action_bar(state.score.to_string().color(Color::LIGHT_PURPLE).bold());
                let mut objective_mut = objectives.single_mut();
                let name = username.to_string();
                let new_score = state.score as i32;
                if let Some(score) = objective_mut.get(&name) {
                    if *score < new_score {
                        objective_mut.insert(name, new_score);
                    }
                } else {
                    objective_mut.insert(name, new_score);
                }
            }
        }
    }
}

fn manage_chunks(mut clients: Query<(&Position, &OldPosition, &mut ChunkLayer), With<Client>>) {
    for (pos, old_pos, mut layer) in &mut clients {
        let old_view = ChunkView::new(old_pos.get().into(), VIEW_DIST);
        let view = ChunkView::new(pos.0.into(), VIEW_DIST);

        if old_view != view {
            for pos in old_view.diff(view) {
                layer.remove_chunk(pos);
            }

            for pos in view.diff(old_view) {
                layer.chunk_entry(pos).or_default();
            }
        }
    }
}

fn generate_next_block(state: &mut GameState, layer: &mut ChunkLayer, in_game: bool) {
    if in_game {
        let removed_block = state.blocks.pop_front().unwrap();
        layer.set_block(removed_block, BlockState::AIR);

        state.score += 1
    }

    let last_pos = *state.blocks.back().unwrap();
    let block_pos = generate_random_block(last_pos, state.target_y);

    if last_pos.y == START_POS.y {
        state.target_y = 0
    } else if last_pos.y < START_POS.y - 30 || last_pos.y > START_POS.y + 30 {
        state.target_y = START_POS.y;
    }

    let mut rng = rand::thread_rng();

    layer.set_block(block_pos, *BLOCK_TYPES.choose(&mut rng).unwrap());
    state.blocks.push_back(block_pos);

    state.last_block_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
}

fn generate_random_block(pos: BlockPos, target_y: i32) -> BlockPos {
    let mut rng = rand::thread_rng();

    let y = match target_y {
        0 => rng.gen_range(-1..2),
        y if y > pos.y => 1,
        _ => -1,
    };
    let z = match y {
        1 => rng.gen_range(1..3),
        -1 => rng.gen_range(2..5),
        _ => rng.gen_range(1..4),
    };
    let x = rng.gen_range(-3..4);

    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}
