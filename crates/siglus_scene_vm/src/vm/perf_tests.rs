use super::*;

fn test_chunk() -> Vec<u8> {
    let mut chunk = vec![0; 33 * 4];
    // In a standalone VM the scene's property count is the shared prefix.
    chunk[14 * 4..15 * 4].copy_from_slice(&1i32.to_le_bytes());
    chunk
}

fn property(value: i32) -> UserPropCell {
    let mut cell = UserPropCell::new(constants::fm::INTLIST, vec![]);
    cell.int_list = vec![value; 16_384];
    cell
}

#[test]
fn cross_scene_calls_move_shared_arrays_and_keep_scene_locals_resident() {
    let chunk = test_chunk();
    let ctx = CommandContext::new(std::env::temp_dir().join("siglus-scope-unit-test"));
    let mut vm = SceneVm::new(SceneStream::new(&chunk).unwrap(), ctx);
    vm.current_scene_no = Some(0);
    vm.user_props.insert(0, property(10));
    vm.user_props.insert(1, property(20));
    let shared_buffer = vm.user_props[&0].int_list.as_ptr();

    let caller = vm.enter_cross_scene_user_prop_scope();
    assert!(!caller.contains_key(&0));
    assert_eq!(vm.user_props[&0].int_list.as_ptr(), shared_buffer);
    vm.current_scene_no = Some(1);
    vm.activate_scene_user_prop_scope(1);
    vm.user_props.get_mut(&0).unwrap().int_list[0] = 11;
    vm.user_props.insert(1, property(30));
    vm.restore_cross_scene_user_prop_scope(caller);
    vm.current_scene_no = Some(0);
    vm.activate_scene_user_prop_scope(0);
    assert_eq!(vm.user_props[&0].int_list.as_ptr(), shared_buffer);
    assert_eq!(vm.user_props[&0].int_list[0], 11);
    assert_eq!(vm.user_props[&1].int_list[0], 20);

    let caller = vm.enter_cross_scene_user_prop_scope();
    vm.current_scene_no = Some(1);
    vm.activate_scene_user_prop_scope(1);
    assert_eq!(vm.user_props[&1].int_list[0], 30);
    vm.restore_cross_scene_user_prop_scope(caller);
}

#[test]
fn frame_checkpoint_restores_execution_but_not_callback_property_writes() {
    let chunk = test_chunk();
    let ctx = CommandContext::new(std::env::temp_dir().join("siglus-callback-unit-test"));
    let mut vm = SceneVm::new(SceneStream::new(&chunk).unwrap(), ctx);
    vm.user_props.insert(0, property(10));
    vm.scene_user_props
        .insert(1, BTreeMap::from([(1, property(20))]));
    vm.int_stack.push(7);
    vm.halted = true;
    let shared_buffer = vm.user_props[&0].int_list.as_ptr();
    let saved = vm.capture_interpreter_exec_state();
    vm.int_stack.push(8);
    vm.halted = false;
    vm.user_props.get_mut(&0).unwrap().int_list[0] = 11;
    vm.scene_user_props
        .get_mut(&1)
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .int_list[0] = 21;
    vm.restore_interpreter_exec_state(saved);
    assert_eq!(vm.int_stack, vec![7]);
    assert!(vm.halted);
    assert_eq!(vm.user_props[&0].int_list.as_ptr(), shared_buffer);
    assert_eq!(vm.user_props[&0].int_list[0], 11);
    assert_eq!(vm.scene_user_props[&1][&1].int_list[0], 21);
}
