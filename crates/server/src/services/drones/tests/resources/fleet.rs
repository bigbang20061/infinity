use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use crate::services::{Call, CallError};
use super::super::fixture::Fixture;

fn call_error(error: CallError) -> anyhow::Error {
    match error {
        CallError::Internal(error) => error,
        CallError::User { msg, .. } => anyhow!("fleet RPC: {msg}"),
    }
}

pub(in crate::services) async fn form_fleet(f: &mut Fixture, other: i64, ship: i64) -> Result<i64> {
    let primary_type = f.world.ball(f.system, f.ship).await.ok_or_else(|| anyhow!("primary ship absent"))?.type_id;
    let other_type = f.world.ball(f.system, ship).await.ok_or_else(|| anyhow!("other ship absent"))?.type_id;
    let kwargs = Value::None;
    let create = Call { service: "fleetObjectHandler", method: "CreateFleet", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::fleets::dispatch(&mut f.client, &create).await.ok_or_else(|| anyhow!("fleet service unavailable"))?.map_err(call_error)?;
    let ids: Vec<i64> = f.client.bound.values().filter_map(|(service, param)|
        (service == "fleetObjectHandler").then(|| param.as_int()).flatten()).collect();
    ensure!(ids.len() == 1, "expected one fleet binding");
    let id = ids[0];
    let params = Value::Int(id);
    for (method, args) in [
        ("Init", vec![Value::Int(i64::from(primary_type)), Value::None, Value::None]),
        ("Invite", vec![Value::Int(other), Value::Int(-1), Value::Int(-1)]),
    ] {
        let call = Call { service: "fleetObjectHandler", method, args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
        crate::services::fleets::dispatch(&mut f.client, &call).await.ok_or_else(|| anyhow!("fleet service unavailable"))?.map_err(call_error)?;
    }
    f.client.session.apply(vec![("charid", Value::Int(other)), ("shipid", Value::Int(ship)), ("fleetid", Value::None)]);
    let args = [Value::Int(i64::from(other_type))];
    let call = Call { service: "fleetObjectHandler", method: "AcceptInvite", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    let result = crate::services::fleets::dispatch(&mut f.client, &call).await.ok_or_else(|| anyhow!("fleet service unavailable")).and_then(|r| r.map_err(call_error));
    f.client.session.apply(vec![("charid", Value::Int(f.owner)), ("shipid", Value::Int(f.ship)), ("fleetid", Value::Int(id))]);
    result?;
    ensure!(crate::services::fleets::same_fleet(f.owner, other), "fleet membership not established");
    Ok(id)
}
