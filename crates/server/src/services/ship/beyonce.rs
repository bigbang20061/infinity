//! The beyonce remote ballpark service; uses the space actor and SDE gate data. NPC workers are outside its scope.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let a = call.args;
    Some(match (call.service, call.method) {
        ("beyonce", "UpdateStateRequest") => send_state(c).await.map(|_| Value::None),
        ("beyonce", "CmdGotoDirection") => {
            let dir = [f(a.first()), f(a.get(1)), f(a.get(2))];
            movement(c, move |b, t, _| {
                b.mode = MODE_GOTO;
                b.goto = dir;
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdAlignTo") => {
            let target = call.kwargs.get("dstID").and_then(|v| v.as_int()).or_else(|| a.first().and_then(|v| v.as_int()));
            movement(c, move |b, t, others| {
                let Some(tp) = target.and_then(|id| others.ball(id)).map(|o| o.pos) else { return vec![] };
                let dir = [tp[0] - b.pos[0], tp[1] - b.pos[1], tp[2] - b.pos[2]];
                b.mode = MODE_GOTO;
                b.goto = dir;
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdStop") => {
            if let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) {
                c.world.command(system, ship, |b, t, _| {
                    b.mode = MODE_STOP;
                    b.speed_fraction = 0.0;
                    vec![event(t, "Stop", vec![Value::Int(b.id)])]
                });
            }
            Ok(Value::None)
        }
        ("beyonce", "CmdSetSpeedFraction") => {
            let frac = f(a.first()).clamp(0.0, 1.0) as f32;
            movement(c, move |b, t, _| {
                b.speed_fraction = frac;
                vec![event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(frac as f64)])]
            })
        }
        ("beyonce", "CmdFollowBall") | ("beyonce", "CmdOrbit") => {
            let target = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let range = f(a.get(1)) as f32;
            let orbit = call.method == "CmdOrbit";
            movement(c, move |b, t, _| {
                b.mode = if orbit { MODE_ORBIT } else { MODE_FOLLOW };
                b.follow_id = target;
                b.follow_range = range;
                let name = if orbit { "Orbit" } else { "FollowBall" };
                let mut ev = vec![event(t, name, vec![Value::Int(b.id), Value::Int(target), Value::Float(range as f64)])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdWarpToStuff") if a.first().and_then(|v| v.as_text()).as_deref() == Some("scan") => {
            let target = a.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            crate::services::exploration::warp_to_scan(c, &target, min_range).await
        }
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot") => {
            let target = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            // 'char': a fleet member's ship.
            let by_pilot = a.first().and_then(|v| v.as_text()).as_deref() == Some("char");
            let found = if let Some(sys) = c.session.int("solarsystemid") {
                c.world.with(sys, move |s| {
                    if by_pilot { s.pilot_ball(target) } else { s.ball(target) }
                        .map(|b| (b.id, b.pos, b.radius as f64, s.warp_in(b.id)))
                }).await.flatten()
            } else {
                None
            };
            let Some((id, pos, radius, warp_in)) = found else {
                return Some(Ok(Value::None));
            };
            // Suns, planets, moons and belts land at their warp-in point, not off the surface.
            let warped = match warp_in {
                Some(point) => warp_to(c, None, point, min_range),
                None => warp_to(c, Some((pos, radius)), pos, min_range),
            };
            // Warping to an asteroid belt brings out its rats.
            if let (Ok(_), false, Some(system)) = (&warped, by_pilot, c.session.int("solarsystemid")) {
                if let Err(e) = crate::services::exploration::belt_rats(c, system, id, pos).await {
                    warn!(user = c.login.user_id, "belt rats at {}: {}", id, crate::services::exploration::err_text(e));
                }
            }
            warped
        }
        ("beyonce", "CmdStargateJump") => {
            let from_gate = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let to_gate = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            stargate_jump(c, from_gate, to_gate).await
        }
        ("beyonce", "CmdDock") | ("beyonce", "CmdTurboDock") => {
            let station = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            dock(c, station).await
        }
        ("beyonce", m) => {
            warn!(user = c.login.user_id, "UNHANDLED {}::{m} args={} kwargs={}", call.service, short(&marshal::Value::Tuple(a.to_vec())), short(call.kwargs));
            Ok(Value::None)
        }
        _ => return None,
    })
}


