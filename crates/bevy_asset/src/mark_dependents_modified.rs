use core::{any::TypeId, marker::PhantomData};

use bevy_app::{Plugin, PostUpdate};
use bevy_ecs::{
    message::MessageReader,
    resource::Resource,
    schedule::{IntoScheduleConfigs, SystemSet},
    system::{Res, ResMut},
};
use bevy_platform::collections::{hash_map::Entry, HashMap, HashSet};

use crate::{Asset, AssetEvent, AssetEventSystems, Assets, UntypedAssetId};

#[derive(Default)]
pub(crate) struct MarkDependentsModifiedPlugin;

impl Plugin for MarkDependentsModifiedPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        app.init_resource::<AssetDependencyGraph>()
            .init_resource::<DependentsModified>()
            .configure_sets(
                PostUpdate,
                (
                    TrackAssetDependencyGraphSystems.after(AssetEventSystems),
                    MarkDependentsModifiedSystems.after(TrackAssetDependencyGraphSystems),
                ),
            )
            .add_systems(
                PostUpdate,
                clear_dependents_modified.after(MarkDependentsModifiedSystems),
            );
    }
}

pub(crate) struct MarkDependentsModifiedAssetPlugin<A: Asset>(PhantomData<A>);

impl<A: Asset> Default for MarkDependentsModifiedAssetPlugin<A> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<A: Asset> Plugin for MarkDependentsModifiedAssetPlugin<A> {
    fn build(&self, app: &mut bevy_app::App) {
        app.add_systems(
            PostUpdate,
            (
                track_asset_dependency_graph::<A>
                    .in_set(TrackAssetDependencyGraphSystems)
                    .ambiguous_with_all(),
                mark_dependents_modified::<A>.in_set(MarkDependentsModifiedSystems),
            ),
        );
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, SystemSet)]
struct TrackAssetDependencyGraphSystems;

#[derive(Debug, Hash, PartialEq, Eq, Clone, SystemSet)]
struct MarkDependentsModifiedSystems;

fn track_asset_dependency_graph<A: Asset>(
    mut asset_dependency_graph: ResMut<AssetDependencyGraph>,
    mut dependents_modified: ResMut<DependentsModified>,
    assets: Res<Assets<A>>,
    mut asset_events: MessageReader<AssetEvent<A>>,
) {
    for asset_event in asset_events.read() {
        let asset_id = match asset_event {
            AssetEvent::Added { id } => {
                // The asset might have been removed after the message was sent.
                let Some(asset) = assets.get(*id) else {
                    continue;
                };
                let untyped_id = id.untyped();
                asset_dependency_graph.add(untyped_id, asset);
                untyped_id
            }
            AssetEvent::Modified { id } => {
                // The asset might have been removed after the message was sent.
                let Some(asset) = assets.get(*id) else {
                    continue;
                };
                let untyped_id = id.untyped();
                asset_dependency_graph.remove(untyped_id);
                asset_dependency_graph.add(untyped_id, asset);
                untyped_id
            }
            AssetEvent::Removed { id } => {
                let untyped_id = id.untyped();
                asset_dependency_graph.remove(untyped_id);
                untyped_id
            }
            AssetEvent::DependenciesModified { id } => id.untyped(),
            AssetEvent::Unused { .. } | AssetEvent::LoadedWithDependencies { .. } => continue,
        };

        if let Some(dependents) = asset_dependency_graph.dependents.get(&asset_id) {
            for dependent in dependents {
                dependents_modified
                    .dependents
                    .entry(dependent.type_id())
                    .or_default()
                    .insert(*dependent);
            }
        }
    }
}

fn mark_dependents_modified<A: Asset>(
    dependents_modified: Res<DependentsModified>,
    mut assets: ResMut<Assets<A>>,
) {
    let Some(dependents) = dependents_modified.dependents.get(&TypeId::of::<A>()) else {
        return;
    };

    for dependent in dependents {
        assets.queue_event(AssetEvent::DependenciesModified {
            id: dependent.typed(),
        });
    }
}

fn clear_dependents_modified(mut dependents_modified: ResMut<DependentsModified>) {
    dependents_modified.dependents.clear();
}

#[derive(Resource, Default)]
struct AssetDependencyGraph {
    dependents: HashMap<UntypedAssetId, HashSet<UntypedAssetId>>,
    dependencies: HashMap<UntypedAssetId, HashSet<UntypedAssetId>>,
}

impl AssetDependencyGraph {
    fn add<A: Asset>(&mut self, dependent: UntypedAssetId, asset: &A) {
        asset.visit_dependencies(&mut |dependency| {
            self.dependents
                .entry(dependency)
                .or_default()
                .insert(dependent);
            self.dependencies
                .entry(dependent)
                .or_default()
                .insert(dependency);
        });
    }

    fn remove(&mut self, dependent: UntypedAssetId) {
        for dependency in self.dependencies.remove(&dependent).unwrap_or_default() {
            if let Entry::Occupied(mut dependents_entry) = self.dependents.entry(dependency) {
                let dependents = dependents_entry.get_mut();
                dependents.remove(&dependent);
                if dependents.is_empty() {
                    dependents_entry.remove();
                }
            }
        }
    }
}

#[derive(Resource, Default)]
pub struct DependentsModified {
    dependents: HashMap<TypeId, HashSet<UntypedAssetId>>,
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec::Vec;
    use bevy_app::App;
    use bevy_ecs::message::Messages;
    use bevy_reflect::TypePath;
    use uuid::Uuid;

    use crate::{AssetApp, AssetPlugin, DirectAssetAccessExt, Handle};

    use super::*;

    #[derive(Asset, TypePath)]
    struct TestAsset1 {
        #[dependency]
        dependency: Vec<Handle<TestAsset2>>,
    }

    #[derive(Asset, TypePath)]
    struct TestAsset2 {}

    fn untyped_asset_id<A: Asset>(id: u128) -> UntypedAssetId {
        UntypedAssetId::Uuid {
            type_id: TypeId::of::<A>(),
            uuid: Uuid::from_u128(id),
        }
    }

    fn handle<A: Asset>(id: u128) -> Handle<A> {
        Handle::Uuid(Uuid::from_u128(id), Default::default())
    }

    #[test]
    fn asset_dependency_graph() {
        let asset_id1 = untyped_asset_id::<TestAsset1>(1);
        let asset_id2 = untyped_asset_id::<TestAsset1>(2);
        let asset_id3 = untyped_asset_id::<TestAsset1>(3);
        let mut asset_dependency_graph = AssetDependencyGraph::default();
        asset_dependency_graph.add(
            asset_id1,
            &TestAsset1 {
                dependency: [handle(4), handle(5)].into(),
            },
        );
        asset_dependency_graph.add(
            asset_id2,
            &TestAsset1 {
                dependency: [handle(4), handle(6)].into(),
            },
        );
        asset_dependency_graph.remove(asset_id2);
        asset_dependency_graph.add(
            asset_id3,
            &TestAsset1 {
                dependency: [handle(5), handle(6)].into(),
            },
        );

        let asset_id4 = untyped_asset_id::<TestAsset2>(4);
        let asset_id5 = untyped_asset_id::<TestAsset2>(5);
        let asset_id6 = untyped_asset_id::<TestAsset2>(6);
        let mut expected_dependencies =
            HashMap::<UntypedAssetId, HashSet<UntypedAssetId>>::default();
        let asset_id1_dependencies = expected_dependencies.entry(asset_id1).or_default();
        asset_id1_dependencies.insert(asset_id4);
        asset_id1_dependencies.insert(asset_id5);
        let asset_id3_dependencies = expected_dependencies.entry(asset_id3).or_default();
        asset_id3_dependencies.insert(asset_id5);
        asset_id3_dependencies.insert(asset_id6);
        assert_eq!(asset_dependency_graph.dependencies, expected_dependencies);

        let mut expected_dependents = HashMap::<UntypedAssetId, HashSet<UntypedAssetId>>::default();
        let asset_id4_dependents = expected_dependents.entry(asset_id4).or_default();
        asset_id4_dependents.insert(asset_id1);
        let asset_id5_dependents = expected_dependents.entry(asset_id5).or_default();
        asset_id5_dependents.insert(asset_id1);
        asset_id5_dependents.insert(asset_id3);
        let asset_id6_dependents = expected_dependents.entry(asset_id6).or_default();
        asset_id6_dependents.insert(asset_id3);
        assert_eq!(asset_dependency_graph.dependents, expected_dependents);
    }

    fn setup_test_app() -> (App, Handle<TestAsset1>, Handle<TestAsset2>) {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default());
        app.init_asset::<TestAsset1>();
        app.init_asset::<TestAsset2>();
        let world = app.world_mut();
        let test_asset2 = world.add_asset(TestAsset2 {});
        let test_asset1 = world.add_asset(TestAsset1 {
            dependency: [test_asset2.clone()].into(),
        });
        app.update();
        (app, test_asset1, test_asset2)
    }

    fn test_modifies_dependent(test_asset_event: fn(Handle<TestAsset2>) -> AssetEvent<TestAsset2>) {
        let (mut app, test_asset1, test_asset2) = setup_test_app();
        let mut test_asset2_messages = app
            .world_mut()
            .resource_mut::<Messages<AssetEvent<TestAsset2>>>();
        test_asset2_messages.write(test_asset_event(test_asset2));

        app.update();

        let test_asset1_messages = app
            .world_mut()
            .resource_mut::<Messages<AssetEvent<TestAsset1>>>();
        let mut test_asset2_messages_iter = test_asset1_messages.iter_current_update_messages();
        assert_eq!(
            test_asset2_messages_iter.next(),
            Some(&AssetEvent::DependenciesModified {
                id: test_asset1.id(),
            }),
        );
        assert_eq!(test_asset2_messages_iter.next(), None);
    }

    #[test]
    fn adding_dependency_modifies_dependent() {
        test_modifies_dependent(|test_asset_event| AssetEvent::Added {
            id: test_asset_event.id(),
        });
    }

    #[test]
    fn modifying_dependency_modifies_dependent() {
        test_modifies_dependent(|test_asset_event| AssetEvent::Modified {
            id: test_asset_event.id(),
        });
    }

    #[test]
    fn removing_dependency_modifies_dependent() {
        test_modifies_dependent(|test_asset_event| AssetEvent::Removed {
            id: test_asset_event.id(),
        });
    }
}
