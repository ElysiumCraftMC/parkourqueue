use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use valence::prelude::*;
use valence::client::Properties;
use valence::protocol::sound::{Sound, SoundCategory};
use valence::scoreboard::*;
use valence::spawn::IsFlat;
use valence::title::SetTitle;
use valence::{CompressionThreshold, ServerSettings};
use valence::entity::player::PlayerEntityBundle;
use valence::player_list::{DisplayName, Listed, PlayerListEntryBundle};
use valence::entity::HeadYaw;

const START_POS: BlockPos = BlockPos::new(0, 100, 0);
const VIEW_DIST: u8 = 10;

const BLOCK_TYPES: [BlockState; 1] = [BlockState::OBSIDIAN];

pub fn main() {
    let connection_mode = match std::env::var("VELOCITY_SECRET") {
        Ok(velocity_secret) => {
            let secret_arc = Arc::from(velocity_secret);
            ConnectionMode::Velocity { secret: secret_arc }
        }
        Err(_) => ConnectionMode::Offline,
    };
    
    let address = std::env::var("ADDRESS")
        .unwrap_or_else(|_| "0.0.0.0:25565".to_string());
    let address: SocketAddr = address.parse().expect("Failed to parse ADDRESS");

    App::new()
        .insert_resource(ServerSettings {
            compression_threshold: CompressionThreshold(-1),
            ..Default::default()
        })
        .insert_resource(NetworkSettings {
            connection_mode,
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
                record_player_movements.after(manage_blocks),
                update_replay_npcs.after(record_player_movements),
                despawn_disconnected_clients,
                apply_custom_skin,
            ),
        )
        .run();
}

#[derive(Debug, Resource)]
struct Globals {
    pub scoreboard_layer: Entity,
    pub highscore: Option<HighScore>,
}

#[derive(Clone, Debug)]
struct PlayerMovement {
    position: [f64; 3],
    yaw: f32,
    pitch: f32,
    timestamp: u128,
}

#[derive(Clone, Debug)]
struct HighScore {
    username: String,
    score: u32,
    seed: u64,
    movements: Vec<PlayerMovement>,
}

#[derive(Component)]
struct GameState {
    blocks: VecDeque<BlockPos>,
    score: u32,
    combo: u32,
    target_y: i32,
    last_block_timestamp: u128,
    seed: u64,
    movements: Vec<PlayerMovement>,
    movement_start_time: u128,
    rng: StdRng,
}

#[derive(Component)]
struct ReplayNpc {
    movements: Vec<PlayerMovement>,
    current_index: usize,
    start_time: u128,
}

#[derive(Component)]
struct ReplayMode {
    original_seed: u64,
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
        highscore: None,
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

        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let state = GameState {
            blocks: VecDeque::new(),
            score: 0,
            combo: 0,
            target_y: 0,
            last_block_timestamp: 0,
            seed,
            movements: Vec::new(),
            movement_start_time: 0,
            rng: StdRng::seed_from_u64(seed),
        };

        let layer = ChunkLayer::new(ident!("the_end"), &dimensions, &biomes, &server);
        let entity_layer = EntityLayer::new(&server);

        commands.entity(entity).insert((state, layer, entity_layer));
    }
}

fn reset_clients(
    mut clients: Query<(
        &mut Client,
        &mut Position,
        &mut Look,
        &mut GameState,
        &mut ChunkLayer,
        &Username,
    )>,
    mut globals: ResMut<Globals>,
) {
    for (mut client, mut pos, mut look, mut state, mut layer, username) in &mut clients {
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
                
                // Check if this is a new global highscore
                let is_new_highscore = if let Some(ref existing_highscore) = globals.highscore {
                    state.score > existing_highscore.score
                } else {
                    state.score > 0
                };
                
                if is_new_highscore {
                    let highscore = HighScore {
                        username: username.to_string(),
                        score: state.score,
                        seed: state.seed,
                        movements: state.movements.clone(),
                    };
                    globals.highscore = Some(highscore);
                    client.send_chat_message(
                        "NEW GLOBAL HIGHSCORE! ".color(Color::GOLD).bold()
                            + format!("Score: {} - Your run has been saved!", state.score).color(Color::GREEN),
                    );
                }
            }

            for pos in ChunkView::new(START_POS.into(), VIEW_DIST).iter() {
                layer.insert_chunk(pos, UnloadedChunk::new());
            }

            state.score = 0;
            state.combo = 0;
            state.seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            state.movements.clear();
            state.movement_start_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();
            state.rng = StdRng::seed_from_u64(state.seed);

            for block in &state.blocks {
                layer.set_block(*block, BlockState::AIR);
            }
            state.blocks.clear();
            state.blocks.push_back(START_POS);
            layer.set_block(START_POS, BlockState::BLACK_WOOL);
            
            // Add gold block for pig spawning
            let gold_block_pos = BlockPos::new(START_POS.x + 2, START_POS.y, START_POS.z);
            layer.set_block(gold_block_pos, BlockState::GOLD_BLOCK);

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
            
            if state.is_added() {
                client.send_chat_message(
                    format!("Parkour seed: {}", state.seed).color(Color::AQUA)
                );
            }
        }
    }
}

fn manage_blocks(
    mut clients: Query<(
        Entity,
        &mut Client,
        &Position,
        &mut GameState,
        &mut ChunkLayer,
        &Username,
    )>,
    mut objectives: Query<&mut ObjectiveScores, With<Objective>>,
    globals: Res<Globals>,
    mut commands: Commands,
) {
    for (entity, mut client, pos, mut state, mut layer, username) in &mut clients {
        let pos_under_player = BlockPos::new(
            (pos.0.x - 0.5).round() as i32,
            pos.0.y as i32 - 1,
            (pos.0.z - 0.5).round() as i32,
        );

        // Check if player is on the gold block (player spawner)
        let gold_block_pos = BlockPos::new(START_POS.x + 2, START_POS.y, START_POS.z);
        if pos_under_player == gold_block_pos {
            let block_type = layer.block(pos_under_player).unwrap_or_default().state;
            if block_type == BlockState::GOLD_BLOCK {
                // Check if there's a global highscore
                if let Some(highscore) = globals.highscore.clone() {
                    // Store original seed and switch to highscore seed
                    let original_seed = state.seed;
                    state.seed = highscore.seed;
                    state.rng = StdRng::seed_from_u64(highscore.seed);
                    
                    // Clear and regenerate the parkour with the highscore seed
                    for block in &state.blocks {
                        layer.set_block(*block, BlockState::AIR);
                    }
                    state.blocks.clear();
                    state.blocks.push_back(START_POS);
                    layer.set_block(START_POS, BlockState::BLACK_WOOL);
                    
                    // Keep the gold block
                    let gold_block_pos = BlockPos::new(START_POS.x + 2, START_POS.y, START_POS.z);
                    layer.set_block(gold_block_pos, BlockState::GOLD_BLOCK);
                    
                    // Generate the same parkour as the highscore run
                    for _ in 0..10 {
                        generate_next_block(&mut state, &mut layer, false);
                    }
                    
                    let player_pos = Position::new([
                        START_POS.x as f64 + 0.5,
                        START_POS.y as f64 + 1.0,
                        START_POS.z as f64 + 0.5,
                    ]);
                    
                    let npc_id = UniqueId::default();
                    
                    // Spawn the player entity with replay component
                    let entity_bundle = PlayerEntityBundle {
                        layer: EntityLayerId(entity),
                        uuid: npc_id,
                        position: player_pos,
                        look: Look::new(0.0, 0.0),
                        head_yaw: HeadYaw(0.0),
                        ..Default::default()
                    };
                    
                    let replay_component = ReplayNpc {
                        movements: highscore.movements,
                        current_index: 0,
                        start_time: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis(),
                    };
                    
                    commands.spawn((entity_bundle, replay_component));
                    
                    // Add replay mode component to the player
                    commands.entity(entity).insert(ReplayMode { original_seed });
                    
                    // Add player list entry so the player is visible
                    // Truncate username to fit 16 character limit
                    let ghost_name = if highscore.username.len() > 10 {
                        format!("{}..Ghost", &highscore.username[..7])
                    } else {
                        format!("{} Ghost", &highscore.username)
                    };
                    
                    commands.spawn(PlayerListEntryBundle {
                        uuid: npc_id,
                        username: Username(ghost_name.chars().take(16).collect::<String>()),
                        display_name: DisplayName(
                            format!("{}'s Ghost ({})", highscore.username, highscore.score)
                                .color(Color::GOLD)
                                .into()
                        ),
                        listed: Listed(false), // Don't show in player list
                        ..Default::default()
                    });
                    
                    client.play_sound(
                        Sound::EntityPlayerLevelup,
                        SoundCategory::Master,
                        pos.0,
                        1.0,
                        1.0,
                    );
                    
                    client.send_chat_message(
                        format!("Loading {}'s highscore run (Score: {}, Seed: {})!", 
                            highscore.username, 
                            highscore.score,
                            highscore.seed
                        ).color(Color::GOLD)
                    );
                } else {
                    client.send_chat_message(
                        "No global highscore recorded yet!".color(Color::RED)
                    );
                }
            }
        }

        // Regular parkour logic
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
    let block_pos = generate_random_block(last_pos, state.target_y, &mut state.rng);

    if last_pos.y == START_POS.y {
        state.target_y = 0
    } else if last_pos.y < START_POS.y - 30 || last_pos.y > START_POS.y + 30 {
        state.target_y = START_POS.y;
    }

    layer.set_block(block_pos, *BLOCK_TYPES.choose(&mut state.rng).unwrap());
    state.blocks.push_back(block_pos);

    state.last_block_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
}

fn generate_random_block(pos: BlockPos, target_y: i32, rng: &mut StdRng) -> BlockPos {

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

fn record_player_movements(
    mut clients: Query<(&Position, &Look, &mut GameState), With<Client>>,
) {
    for (pos, look, mut state) in &mut clients {
        if state.score > 0 {
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();
            
            let movement = PlayerMovement {
                position: [pos.0.x, pos.0.y, pos.0.z],
                yaw: look.yaw,
                pitch: look.pitch,
                timestamp: current_time - state.movement_start_time,
            };
            
            state.movements.push(movement);
        }
    }
}

fn update_replay_npcs(
    mut npcs: Query<(Entity, &mut Position, &mut Look, &mut HeadYaw, &mut ReplayNpc)>,
    mut commands: Commands,
) {
    for (entity, mut pos, mut look, mut head_yaw, mut replay) in &mut npcs {
        // Check if movements vector is empty
        if replay.movements.is_empty() {
            commands.entity(entity).despawn();
            continue;
        }
        
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        
        let elapsed = current_time.saturating_sub(replay.start_time);
        
        // Find the appropriate movement frame
        while replay.current_index < replay.movements.len().saturating_sub(1) {
            if replay.movements[replay.current_index + 1].timestamp <= elapsed {
                replay.current_index += 1;
            } else {
                break;
            }
        }
        
        if replay.current_index >= replay.movements.len() {
            // Replay finished, despawn the NPC
            commands.entity(entity).despawn();
            continue;
        }
        
        // Interpolate between movements for smooth playback
        let current_movement = &replay.movements[replay.current_index];
        
        if replay.current_index < replay.movements.len() - 1 {
            let next_movement = &replay.movements[replay.current_index + 1];
            let time_diff = next_movement.timestamp - current_movement.timestamp;
            let time_since_current = elapsed.saturating_sub(current_movement.timestamp);
            let t = if time_diff > 0 {
                (time_since_current as f64) / (time_diff as f64)
            } else {
                1.0
            };
            let t = t.clamp(0.0, 1.0);
            
            // Interpolate position
            pos.0.x = current_movement.position[0] + (next_movement.position[0] - current_movement.position[0]) * t;
            pos.0.y = current_movement.position[1] + (next_movement.position[1] - current_movement.position[1]) * t;
            pos.0.z = current_movement.position[2] + (next_movement.position[2] - current_movement.position[2]) * t;
            
            // Interpolate rotation
            look.yaw = current_movement.yaw + (next_movement.yaw - current_movement.yaw) * t as f32;
            look.pitch = current_movement.pitch + (next_movement.pitch - current_movement.pitch) * t as f32;
            head_yaw.0 = look.yaw;
        } else {
            // Use the last movement
            pos.0.x = current_movement.position[0];
            pos.0.y = current_movement.position[1];
            pos.0.z = current_movement.position[2];
            look.yaw = current_movement.yaw;
            look.pitch = current_movement.pitch;
            head_yaw.0 = look.yaw;
        }
    }
}

fn apply_custom_skin(mut query: Query<&mut Properties, (Added<Properties>, Without<Client>)>) {
    for mut props in &mut query {
        props.set_skin(
            "ewogICJ0aW1lc3RhbXAiIDogMTY5MTcwNjU3MzE1NiwKICAicHJvZmlsZUlkIiA6ICJlODgyNzRlYjNmNTE0ZDYwYmMxYWQ5NTQ4MTIxODMwMyIsCiAgInByb2ZpbGVOYW1lIiA6ICJBbmltYWxUaGVHYW1lciIsCiAgInNpZ25hdHVyZVJlcXVpcmVkIiA6IHRydWUsCiAgInRleHR1cmVzIiA6IHsKICAgICJTS0lOIiA6IHsKICAgICAgInVybCIgOiAiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS8xZGUyYzgzZjhmNGZiMzgwNjlmNTVmNTJlNGY4ZWU1ZjA4NjcyMjllYWQ5MWI3ZTc5ZGVmNzU0YjcwZWE5NDMzIiwKICAgICAgIm1ldGFkYXRhIiA6IHsKICAgICAgICAibW9kZWwiIDogInNsaW0iCiAgICAgIH0KICAgIH0KICB9Cn0=",
            "k/g8JTYB0A5O+h8+XSdw3QFEVHnzomDsGl6eubV/sE396yAL7E4qCT24r3Uv88YYforuET1BXG0GBOewcij3uMajm+mc/P7v+0+C+NSS9g5dpSs2e9MdeGZBgDEr1kTnXzQmayZUvLGitW23GuRDHdVHx76JZpxBk3q0VsjgncNs6UVZwfYNCaUGZZx38bqG5FXGxE0MfFHKiJawKwWRaoAbHjrfsByLipIKUhssUF3pt+HPWbgaOD2rO0EOLBrGzvEnu9oeLPH4tqdlvurjGrdpM4wKCmS3j8K91OBTABciVR9xt0fRnhbL4JoZuLK+iefNXx8nBCVEOm9sNk4pXHNWZvKEkqMb3jvpxuYHsSZPm0IdN+74FEmjHy0sY/7+ZG/h/IUHs4CyrPAtR/rqON6MG8nVVBxUq4kWV+2Xj+U+O02gQUVFqMM77AqArRsPIkeFIgVQ6+WvBZYXuRe1Ryo6qwjmYGc4AeTZTtvafzv8vfAMFfJJmT69nkTTDO5hAtDTUnCd86nNFQ3qijdO9CW7OFDyysb9M0a1O7pQ7Nu10rkNwY+6uTfKoATtT80+RoMzvKwcIAG4cY+PR5jhsKP+sf+AEymovD+cPVnLOuZQ6bAyKW6yjf9Xd0vyirCgNaU1CGmDE1mihGK2kC0fm11RaoDbyKvMcLKAq+OFos0=",
        );
    }
}
