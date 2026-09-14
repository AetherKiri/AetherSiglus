use anyhow::Result;

use crate::runtime::forms::prop_access;
use crate::runtime::{CommandContext, Value};

pub fn dispatch(ctx: &mut CommandContext, form_id: u32, args: &[Value]) -> Result<bool> {
    let parsed = prop_access::parse_element_chain_ctx(ctx, form_id, args);
    let (chain_pos, chain) = match parsed {
        Some((pos, ch)) if ch.len() >= 2 => (Some(pos), Some(ch)),
        _ => (None, None),
    };

    if let Some(chain) = chain {
        let op = chain[1];
        let params = if let Some(pos) = chain_pos {
            prop_access::script_args(args, pos)
        } else {
            &[]
        };
        // Aether is an offline emulator, not the game's Steamworks client.
        // Accept the native void commands without claiming account sync or
        // loading a game's SDK. In particular RESET must never affect Steam.
        if ctx.ids.steam_set_achievement != 0 && op == ctx.ids.steam_set_achievement {
            let _name = params.first().and_then(Value::as_str);
            ctx.push(Value::Int(0));
            return Ok(true);
        }

        if ctx.ids.steam_reset_all_status != 0 && op == ctx.ids.steam_reset_all_status {
            ctx.push(Value::Int(0));
            return Ok(true);
        }

        prop_access::store_or_push_prop(ctx, form_id, op, chain_pos.unwrap(), args);
        return Ok(true);
    }

    prop_access::dispatch_stateful_form(ctx, form_id, args);
    Ok(true)
}
