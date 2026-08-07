use anyhow::{bail, Result};

use crate::runtime::input::JOYPAD_KEY_COUNT;
use crate::runtime::{CommandContext, Value};

/// SiglusEngine 1.1.141.2 adds GLOBAL.188 as a fixed joypad-key array.
///
/// Recovered bytecode shape:
///     [188, 0, ELM_ARRAY, key_no, KEY_OP]
///
/// The terminal operations are the ordinary KEY BUTTON queries.  Like
/// `tnm_command_proc_key` in the original engine they CHECK edge stock; they do
/// not consume it.
pub fn dispatch(ctx: &mut CommandContext, _args: &[Value]) -> Result<bool> {
    let Some(vm_call) = ctx.vm_call.as_ref() else {
        return Ok(false);
    };
    let chain = &vm_call.element;
    if chain.len() < 4 || chain[0] != ctx.ids.form_global_joypad as i32 {
        return Ok(false);
    }

    // Newer registration exposes property 0 as the joypad KEY array.
    if chain[1] != 0 || chain[2] != ctx.ids.elm_array {
        return Ok(false);
    }

    // Keep property traversal usable by the VM in the same way as KEYLIST:
    // JOYPAD.KEY[-1,index] without a terminal KEY command denotes the item.
    if chain.len() == 4 {
        ctx.push(Value::Element(chain.to_vec()));
        return Ok(true);
    }

    let key_no = chain[3];
    let op = chain[4] as i64;
    if key_no < 0 || key_no as usize >= JOYPAD_KEY_COUNT {
        bail!("joypad key index out of range: {key_no}");
    }
    let key_no = key_no as usize;

    let v = if op == ctx.ids.key_op_on_down as i64 {
        ctx.script_input.joypad_down_stock(key_no)
    } else if op == ctx.ids.key_op_on_up as i64 {
        ctx.script_input.joypad_up_stock(key_no)
    } else if op == ctx.ids.key_op_on_down_up as i64 {
        ctx.script_input.joypad_down_up_stock(key_no)
    } else if op == ctx.ids.key_op_is_down as i64 {
        ctx.script_input.joypad_is_down(key_no)
    } else if op == ctx.ids.key_op_is_up as i64 {
        !ctx.script_input.joypad_is_down(key_no)
    } else {
        bail!("unsupported joypad key operation: {op}");
    };

    ctx.push(Value::Int(if v { 1 } else { 0 }));
    Ok(true)
}
