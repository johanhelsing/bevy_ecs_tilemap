use std::marker::PhantomData;

use bevy::{
    asset::uuid_handle,
    core_pipeline::core_2d::Transparent2d,
    ecs::{component::ComponentId, entity_disabling::DefaultQueryFilters, world::DeferredWorld},
    image::ImageSamplerDescriptor,
    mesh::MeshVertexAttribute,
    platform::collections::HashSet,
    prelude::*,
    render::{
        MainWorld, Render, RenderApp, RenderSystems,
        extract_resource::{ExtractResource, extract_resource},
        render_phase::AddRenderCommand,
        render_resource::{FilterMode, SpecializedRenderPipelines, VertexFormat},
        sync_world::RenderEntity,
    },
    shader::load_shader_library,
};

#[cfg(not(feature = "atlas"))]
use bevy::render::renderer::RenderDevice;
#[cfg(not(feature = "atlas"))]
use bevy::render::texture::GpuImage;
use extract::remove_changed;

use crate::tiles::{TilePos, TileStorage};
use crate::{
    prelude::TilemapTexture,
    render::{
        material::{MaterialTilemapPlugin, StandardTilemapMaterial},
        prepare::{MeshUniformResource, TilemapUniformResource},
    },
};

use self::{
    chunk::RenderChunk2dStorage, draw::DrawTilemap, pipeline::TilemapPipeline,
    queue::ImageBindGroups,
};

mod chunk;
mod draw;
mod extract;
pub mod material;
mod pipeline;
pub(crate) mod prepare;
mod queue;

#[cfg(not(feature = "atlas"))]
mod texture_array_cache;

#[cfg(not(feature = "atlas"))]
use self::extract::ExtractedTilemapTexture;
#[cfg(not(feature = "atlas"))]
pub(crate) use self::texture_array_cache::TextureArrayCache;

#[derive(Copy, Clone, Debug, Component)]
pub(crate) struct ExtractedFilterMode(FilterMode);

#[derive(Resource, Deref)]
pub struct DefaultSampler(ImageSamplerDescriptor);

/// Size of the chunks used to render the tilemap.
///
/// Initialized from [`TilemapRenderSettings`](crate::map::TilemapRenderSettings) resource, if
/// provided. Otherwise, defaults to `64 x 64`.
#[derive(Debug, Copy, Clone, Deref)]
pub(crate) struct RenderChunkSize(UVec2);

impl RenderChunkSize {
    pub const fn new(chunk_size: UVec2) -> RenderChunkSize {
        RenderChunkSize(chunk_size)
    }

    /// Calculates the index of the chunk this tile is in.
    #[inline]
    pub fn map_tile_to_chunk(&self, tile_position: &TilePos) -> UVec2 {
        let tile_pos: UVec2 = tile_position.into();
        tile_pos / self.0
    }

    /// Calculates the index of this tile within the chunk.
    #[inline]
    pub fn map_tile_to_chunk_tile(&self, tile_position: &TilePos, chunk_position: &UVec2) -> UVec2 {
        let tile_pos: UVec2 = tile_position.into();
        tile_pos - (*chunk_position * self.0)
    }
}

pub struct TilemapRenderingPlugin;

pub const COLUMN_EVEN_HEX: Handle<Shader> = uuid_handle!("d11ea18c-32ef-4b16-ba20-c7b092e46ce8");
pub const COLUMN_HEX: Handle<Shader> = uuid_handle!("9161d191-94ff-48f7-8e46-6950bcad1c7a");
pub const COLUMN_ODD_HEX: Handle<Shader> = uuid_handle!("6806e648-498f-4aaf-a4cc-59db167b2e2b");
pub const COMMON: Handle<Shader> = uuid_handle!("0f11250b-3108-4417-9691-502b6daad0c5");
pub const DIAMOND_ISO: Handle<Shader> = uuid_handle!("c21075c7-3455-4db0-9e70-af1d3c5dd535");
pub const MESH_OUTPUT: Handle<Shader> = uuid_handle!("525be111-6731-4c38-be46-573a615a5e83");
pub const ROW_EVEN_HEX: Handle<Shader> = uuid_handle!("b496c0e9-e57c-4a13-88a3-3b7a5033fe89");
pub const ROW_HEX: Handle<Shader> = uuid_handle!("04a9c819-45e0-42d3-9cea-8b9e5440ca00");
pub const ROW_ODD_HEX: Handle<Shader> = uuid_handle!("9962f145-0937-44f4-98f5-0cd5deadd643");
pub const STAGGERED_ISO: Handle<Shader> = uuid_handle!("da349823-a307-44a5-ab78-6276c7cb582a");
pub const SQUARE: Handle<Shader> = uuid_handle!("6db56afb-a562-4e3c-b459-486a6d5c12ae");
pub const TILEMAP_VERTEX_OUTPUT: Handle<Shader> =
    uuid_handle!("49b568da-6c5a-4936-a3c8-d5dd6b894f92");

impl Plugin for TilemapRenderingPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(feature = "atlas"))]
        app.add_systems(Update, set_texture_to_copy_src);

        app.init_resource::<RemovedEntities>();
        app.add_observer(on_remove_tile);
        app.add_observer(on_remove_tilemap);

        app.add_plugins(MaterialTilemapPlugin::<StandardTilemapMaterial>::default());

        app.world_mut()
            .resource_mut::<Assets<StandardTilemapMaterial>>()
            .insert(
                Handle::<StandardTilemapMaterial>::default().id(),
                StandardTilemapMaterial::default(),
            )
            .unwrap();

        app.init_resource::<ModifiedImageIds>()
            .add_systems(Update, collect_modified_image_asset_messages);

        load_shader_library!(app, "shaders/common.wesl");
        load_shader_library!(app, "shaders/column_even_hex.wesl");
        load_shader_library!(app, "shaders/column_hex.wesl");
        load_shader_library!(app, "shaders/column_odd_hex.wesl");
        load_shader_library!(app, "shaders/diamond_iso.wesl");
        load_shader_library!(app, "shaders/row_even_hex.wesl");
        load_shader_library!(app, "shaders/row_hex.wesl");
        load_shader_library!(app, "shaders/row_odd_hex.wesl");
        load_shader_library!(app, "shaders/row_hex.wesl");
        load_shader_library!(app, "shaders/mesh_output.wesl");
        load_shader_library!(app, "shaders/square.wesl");
        load_shader_library!(app, "shaders/staggered_iso.wesl");
        load_shader_library!(app, "shaders/tilemap_vertex_output.wesl");
        load_shader_library!(app, "shaders/tilemap_vertex.wesl");
        load_shader_library!(app, "shaders/tilemap_fragment.wesl");
    }

    fn finish(&self, app: &mut App) {
        // Every disabling component is registered by now, the app's own too.
        let disabling: Vec<ComponentId> = app
            .world()
            .resource::<DefaultQueryFilters>()
            .disabling_ids()
            .collect();
        app.world_mut()
            .spawn(Observer::new(on_disable_tile).with_components(disabling.clone()));
        app.world_mut()
            .spawn(Observer::new(on_enable_tile).with_components(disabling));

        let sampler = app.get_added_plugins::<ImagePlugin>().first().map_or_else(
            || ImagePlugin::default_nearest().default_sampler,
            |plugin| plugin.default_sampler.clone(),
        );

        let render_app = match app.get_sub_app_mut(RenderApp) {
            Some(render_app) => render_app,
            None => return,
        };

        render_app.init_resource::<TilemapPipeline>();

        #[cfg(not(feature = "atlas"))]
        render_app
            .init_resource::<TextureArrayCache>()
            .add_systems(
                Render,
                prepare_textures.in_set(RenderSystems::PrepareAssets),
            )
            .add_systems(Render, texture_array_cache::remove_modified_textures);

        render_app
            .insert_resource(DefaultSampler(sampler))
            .insert_resource(RenderChunk2dStorage::default())
            .init_resource::<RemovedEntities>()
            .add_systems(
                ExtractSchedule,
                (
                    extract_removed,
                    extract::extract,
                    extract_resource::<ModifiedImageIds, RenderApp, ()>,
                ),
            )
            .add_systems(
                Render,
                (prepare::prepare_removal, prepare::prepare)
                    .chain()
                    .in_set(RenderSystems::PrepareAssets),
            )
            .add_systems(
                Render,
                queue::queue_transform_bind_group.in_set(RenderSystems::PrepareBindGroups),
            )
            .add_systems(Render, remove_changed.in_set(RenderSystems::Cleanup))
            .init_resource::<ImageBindGroups>()
            .init_resource::<SpecializedRenderPipelines<TilemapPipeline>>()
            .init_resource::<MeshUniformResource>()
            .init_resource::<TilemapUniformResource>()
            .init_resource::<ModifiedImageIds>();

        render_app.add_render_command::<Transparent2d, DrawTilemap>();
    }
}

pub fn set_texture_to_copy_src(
    mut images: ResMut<Assets<Image>>,
    texture_query: Query<&TilemapTexture>,
) {
    // quick and dirty, run this for all textures anytime a texture component is created.
    for texture in texture_query.iter() {
        texture.set_images_to_copy_src(&mut images)
    }
}

/// Stores the index of a uniform inside of [`ComponentUniforms`].
#[derive(Component)]
pub struct DynamicUniformIndex<C: Component> {
    index: u32,
    marker: PhantomData<C>,
}

impl<C: Component> DynamicUniformIndex<C> {
    #[inline]
    pub fn index(&self) -> u32 {
        self.index
    }
}

pub const ATTRIBUTE_POSITION: MeshVertexAttribute =
    MeshVertexAttribute::new("Position", 229221259, VertexFormat::Float32x4);
pub const ATTRIBUTE_TEXTURE: MeshVertexAttribute =
    MeshVertexAttribute::new("Texture", 222922753, VertexFormat::Float32x4);
pub const ATTRIBUTE_COLOR: MeshVertexAttribute =
    MeshVertexAttribute::new("Color", 231497124, VertexFormat::Float32x4);

/// The render entities of the tiles and maps that went since the last
/// extraction, for the render world to take out of its chunks.
///
/// A resource the extraction drains, not an entity per removal cleared in a
/// main-world schedule: a removal made in that schedule before the clear
/// (an app's own system in `First`, before `TilemapFirstSet`) was cleared
/// before any extraction saw it. Its map's chunks stayed under its entity
/// index, and the next map to take the index drew into chunks that still
/// named the dead map, which draws nothing.
#[derive(Resource, Default)]
pub(crate) struct RemovedEntities {
    pub(crate) tiles: Vec<Entity>,
    pub(crate) maps: Vec<Entity>,
}

/// A tile that goes leaves its chunk. Read from the world, as for a map:
/// a tile despawned while disabled is skipped by a query.
fn on_remove_tile(removed: On<Remove<TilePos>>, mut world: DeferredWorld) {
    if let Some(&render_entity) = world.get::<RenderEntity>(removed.entity) {
        world
            .resource_mut::<RemovedEntities>()
            .tiles
            .push(render_entity.id());
    }
}

/// A tile that gains a disabling component (`Disabled`, or one the app
/// registers, such as a rollback crate's despawn mark) leaves its chunk as a
/// removed tile does. Extraction no longer sees it, so nothing else would
/// take it out.
fn on_disable_tile(add: On<Add<()>>, mut world: DeferredWorld) {
    let entity = add.event_target();
    if world.get::<TilePos>(entity).is_none() {
        return;
    }
    if let Some(&render_entity) = world.get::<RenderEntity>(entity) {
        world
            .resource_mut::<RemovedEntities>()
            .tiles
            .push(render_entity.id());
    }
}

/// A tile that loses one is extracted again, which puts it back in its
/// chunk.
fn on_enable_tile(remove: On<Remove<()>>, mut world: DeferredWorld) {
    if let Some(mut pos) = world.get_mut::<TilePos>(remove.event_target()) {
        pos.set_changed();
    }
}

/// A map that goes takes its chunks with it. Read from the world, not a
/// query: a map despawned while disabled (a rollback crate's despawn mark)
/// is skipped by a query's default filters, and its chunks would stay under
/// its entity index for the next map that takes the index to draw into.
fn on_remove_tilemap(removed: On<Remove<TileStorage>>, mut world: DeferredWorld) {
    if let Some(&render_entity) = world.get::<RenderEntity>(removed.entity) {
        world
            .resource_mut::<RemovedEntities>()
            .maps
            .push(render_entity.id());
    }
}

/// Moves the removals made since the last extraction to the render world,
/// each one exactly once, whenever in the frame it was made.
fn extract_removed(mut main_world: ResMut<MainWorld>, mut removed: ResMut<RemovedEntities>) {
    let mut main = main_world.resource_mut::<RemovedEntities>();
    removed.tiles.append(&mut main.tiles);
    removed.maps.append(&mut main.maps);
}

#[cfg(not(feature = "atlas"))]
fn prepare_textures(
    render_device: Res<RenderDevice>,
    mut texture_array_cache: ResMut<TextureArrayCache>,
    extracted_tilemap_textures: Query<&ExtractedTilemapTexture>,
    render_images: Res<bevy::render::render_asset::RenderAssets<GpuImage>>,
) {
    for extracted_texture in extracted_tilemap_textures.iter() {
        texture_array_cache.add_extracted_texture(extracted_texture);
    }

    texture_array_cache.prepare(&render_device, &render_images);
}

/// Resource to hold the ids of modified Image assets of a single frame.
#[derive(Resource, ExtractResource, Clone, Default)]
#[extract_app(RenderApp)]
pub struct ModifiedImageIds(HashSet<AssetId<Image>>);

impl ModifiedImageIds {
    // Determines whether `texture` contains any handles of modified images.
    pub fn is_texture_modified(&self, texture: &TilemapTexture) -> bool {
        texture
            .image_handles()
            .iter()
            .any(|&image| self.0.contains(&image.id()))
    }
}

/// A system to collect the asset events of modified images for one frame.
/// AssetEvents cannot be read from the render sub-app, so this system packs
/// them up into a convenient resource which can be extracted for rendering.
pub fn collect_modified_image_asset_messages(
    mut asset_messages: MessageReader<AssetEvent<Image>>,
    mut modified_image_ids: ResMut<ModifiedImageIds>,
) {
    modified_image_ids.0.clear();

    for asset_event in asset_messages.read() {
        let id = match asset_event {
            AssetEvent::Modified { id } => id,
            _ => continue,
        };
        modified_image_ids.0.insert(*id);
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::entity_disabling::Disabled;

    use super::*;
    use crate::map::TilemapSize;

    fn main_world() -> World {
        let mut world = World::new();
        world.init_resource::<RemovedEntities>();
        world.add_observer(on_remove_tilemap);
        world.add_observer(on_remove_tile);
        world
    }

    /// Runs the extraction of removals over `main`, as the render app does
    /// once a frame, and returns what the render world was given.
    fn extract(main: World, render: &mut World) -> World {
        let mut main_world = MainWorld::default();
        *main_world = main;
        render.insert_resource(main_world);
        render.run_system_cached(extract_removed).unwrap();
        std::mem::take(&mut *render.remove_resource::<MainWorld>().unwrap())
    }

    /// A map despawned while disabled still sends its removal, so its
    /// chunks go with it.
    #[test]
    fn a_map_despawned_while_disabled_takes_its_chunks() {
        let mut world = main_world();
        let render = world.spawn_empty().id();
        let map = world
            .spawn((
                TileStorage::empty(TilemapSize { x: 1, y: 1 }),
                RenderEntity::from(render),
            ))
            .id();
        world.entity_mut(map).insert(Disabled);
        world.entity_mut(map).despawn();
        assert_eq!(world.resource::<RemovedEntities>().maps, vec![render]);
    }

    /// A tile despawned while disabled still sends its removal.
    #[test]
    fn a_tile_despawned_while_disabled_leaves_its_chunk() {
        let mut world = main_world();
        let render = world.spawn_empty().id();
        let tile = world
            .spawn((TilePos::new(0, 0), RenderEntity::from(render)))
            .id();
        world.entity_mut(tile).insert(Disabled);
        world.entity_mut(tile).despawn();
        assert_eq!(world.resource::<RemovedEntities>().tiles, vec![render]);
    }

    /// A map despawned in `First`, before this crate's own systems there,
    /// keeps its removal through the rest of the frame, for the extraction
    /// that follows. A clear in `First` once took it before any extraction
    /// saw it (an editor's load runs there), and the map's chunks stayed.
    #[test]
    fn a_map_despawned_early_in_the_frame_keeps_its_removal() {
        #[derive(Resource)]
        struct Doomed(Entity);

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<bevy::shader::Shader>()
            .init_asset::<Image>();
        // The rendering half is added only beside a render app; an empty
        // one is enough for its main-world part.
        app.insert_sub_app(RenderApp, bevy::app::SubApp::new());
        app.add_plugins(crate::TilemapPlugin);
        app.add_systems(
            First,
            (|mut commands: Commands, doomed: Res<Doomed>| {
                commands.entity(doomed.0).despawn();
            })
            .before(crate::TilemapFirstSet),
        );
        let render = app.world_mut().spawn_empty().id();
        let map = app
            .world_mut()
            .spawn((
                TileStorage::empty(TilemapSize { x: 1, y: 1 }),
                RenderEntity::from(render),
            ))
            .id();
        app.insert_resource(Doomed(map));
        app.update();
        assert_eq!(app.world().resource::<RemovedEntities>().maps, vec![render]);
    }

    /// An extraction hands each removal to the render world once.
    #[test]
    fn a_removal_reaches_the_render_world_once() {
        let mut main = main_world();
        let mut render = World::new();
        render.init_resource::<RemovedEntities>();
        let render_map = render.spawn_empty().id();
        let map = main
            .spawn((
                TileStorage::empty(TilemapSize { x: 1, y: 1 }),
                RenderEntity::from(render_map),
            ))
            .id();
        main.despawn(map);
        let main = extract(main, &mut render);
        assert_eq!(render.resource::<RemovedEntities>().maps, vec![render_map]);
        render.resource_mut::<RemovedEntities>().maps.clear();
        extract(main, &mut render);
        assert!(render.resource::<RemovedEntities>().maps.is_empty());
    }
}
