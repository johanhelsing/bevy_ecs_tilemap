//! TilemapPlugin must not panic when added to an app without RenderPlugin.
//! The `render` feature is enabled (it's the default), but no actual renderer
//! is present, only MinimalPlugins + AssetPlugin.

#[cfg(feature = "render")]
#[test]
fn tilemap_plugin_without_render_plugin() {
    use bevy::prelude::*;
    use bevy_ecs_tilemap::prelude::*;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin::default())
        .add_plugins(TilemapPlugin);
    app.finish();
    app.cleanup();
    app.update();
}
