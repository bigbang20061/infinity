-- The MOTD is persistent per script and is sent when a ballpark is bound.
register.command{
    name = "motd",
    help = messages.motd_help,
    gm = true,
    fn = function(ctx, args)
        if #args == 0 then
            return store.get("motd") or messages.motd_empty
        end
        if args[1] ~= "set" or #args < 2 then
            return messages.motd_help
        end
        store.set("motd", table.concat(args, " ", 2))
        return messages.motd_saved
    end,
}

register.event("BallparkBound", function(ctx)
    local text = store.get("motd")
    if text and text ~= "" then
        ctx:notify("OnRemoteMessage", eve.str(messages.custom_notify), eve.dict{notify = eve.unicode(text)})
    end
end)
