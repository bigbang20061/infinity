-- Fixed responses for unmodelled automatic client calls. Conditional feature
-- gates and consumers are documented in docs/lua.md.
register.rpc("infoGatheringMgr", "GetStateAndConfig", function(ctx, args, kwargs)
    return eve.keyval{
        isEnabled = eve.bool(false),
        infoTypes = eve.list{},
        clientWorkerInterval = eve.int(0),
        infoTypesOncePerRun = eve.list{},
        infoTypeAggregates = eve.dict{},
        infoTypeParameters = eve.dict{},
    }
end)

register.rpc("characterRenameMgr", "GetDetails", function(ctx, args, kwargs)
    return eve.dict{byGM = eve.int(0)}
end)

register.rpc("localizationServer", "GetAllTextChanges", function(ctx, args, kwargs)
    return eve.none
end)

register.rpc("localizationServer", "UpdateLocalizationQAWrap", function(ctx, args, kwargs)
    return eve.none
end)

register.rpc("eveacsMgr", "IsCanary", function(ctx, args, kwargs)
    return eve.bool(false)
end)
