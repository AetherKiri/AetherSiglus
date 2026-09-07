use super::*;

#[test]
fn scene_cursors_and_savepoints_share_names_but_keep_independent_execution_state() {
    let mut chunk = vec![0; 132 + 32];
    chunk[4..8].copy_from_slice(&132i32.to_le_bytes());
    chunk[8..12].copy_from_slice(&32i32.to_le_bytes());
    let mut stream = SceneStream::new(&chunk).unwrap();
    Arc::make_mut(&mut stream.scn_cmd_name_map).insert(0, "scene_command".into());
    Arc::make_mut(&mut stream.scn_prop_name_map).insert(1, "scene_property".into());
    Arc::make_mut(&mut stream.call_prop_name_map).insert(2, "call_property".into());
    let cursor = stream.clone();
    assert!(Arc::ptr_eq(&stream.scn_cmd_name_map, &cursor.scn_cmd_name_map));
    assert!(Arc::ptr_eq(&stream.scn_prop_name_map, &cursor.scn_prop_name_map));
    assert!(Arc::ptr_eq(&stream.call_prop_name_map, &cursor.call_prop_name_map));
    stream.set_prg_cntr(17).unwrap();
    assert_eq!(cursor.pc, 0);

    let ctx = CommandContext::new(std::env::temp_dir().join("siglus-metadata-unit-test"));
    let mut vm = SceneVm::new(stream, ctx);
    Arc::make_mut(&mut vm.call_cmd_names).insert(3, "include_command".into());
    vm.int_stack.push(7);
    let names = vm.user_cmd_names.clone();
    let include_names = vm.call_cmd_names.clone();
    let resume = vm.make_resume_point();
    assert!(Arc::ptr_eq(&names, &vm.stream.scn_cmd_name_map));
    assert!(Arc::ptr_eq(&names, &resume.user_cmd_names));
    assert!(Arc::ptr_eq(&include_names, &resume.call_cmd_names));

    vm.user_cmd_names = Arc::default();
    vm.call_cmd_names = Arc::default();
    vm.stream.pc = 0;
    vm.int_stack.push(8);
    vm.restore_resume_point(resume);
    assert!(Arc::ptr_eq(&names, &vm.user_cmd_names));
    assert!(Arc::ptr_eq(&include_names, &vm.call_cmd_names));
    assert_eq!(vm.stream.pc, 17);
    assert_eq!(vm.int_stack, vec![7]);
    assert_eq!(vm.user_cmd_names[&0], "scene_command");
    assert_eq!(vm.call_cmd_names[&3], "include_command");
}
