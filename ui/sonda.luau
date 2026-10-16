-- Sonda's logic: it keeps where you are and what is chosen, and talks to
-- `sonda bridge` (a JSON line each way), which does the work: the folders,
-- the small pictures, the copies, the trash, and the searches.

local ROWS_GRID, ROWS_LINES = 12, 30   -- rows the scene holds (rows max 30)
local MAX_COLS = 10         -- tiles a row holds (cells max 10)
local GRID_H, LINES_H = 168, 38

local KIND_NAME = {
    folder = "Folder", image = "Picture", screenshot = "Screenshot", pdf = "PDF", doc = "Document",
    audio = "Sound", video = "Video", code = "Code", archive = "Archive", other = "File",
}
local FILTERS = { "all", "image", "pdf", "doc", "audio", "video", "code" }
local PLACE_ICONS = {
    home = "user-home", desktop = "user-desktop", documents = "folder-documents", downloads = "folder-download",
    pictures = "folder-pictures", music = "folder-music", videos = "folder-videos", projects = "folder-projects",
    trash = "user-trash", computer = "computer",
}

local bridge = nil
-- What Ctrl+Z says it undid.
local UNDONE = {
    copy = "the copy", move = "the move", rename = "the new name", trash = "into the trash",
    mkdir = "the new folder", compress = "the packing", extract = "the unpacking",
}
local cfg = { folders = {}, code_folders = {} }   -- what the daemon reads (its config.json)
local s = {
    path = "", items = {}, back = {}, fwd = {},
    cols = 6, view = "grid", sort = "name", hidden = false,
    picked = {}, anchor = nil, chosen = -1,
    mode = "browse", hits = {}, filter = 1, scope = "everywhere", searchgen = 0, lastq = "",
    clip = nil, thumbs = {}, places = {}, home = "", trash = "",
    first = 0, token = 0, menu_index = nil, asking = nil, editing = false,
    lastpress = nil, lastpressed = false, refresh_due = false, status_due = false,
    jobs = {}, drives = {}, mounting = {}, pick = nil, pickfilter = 1, apps = {}, writable = true,
}

local function send(t)
    if bridge then write(bridge, json.encode(t) .. "\n") end
end

local function toast(msg)
    text.toast = msg
    fact.toasted = fact.toasted + 1
end

local function basename(p)
    return (p:match("([^/]+)/?$")) or p
end

local function dirname(p)
    local d = p:match("^(.*)/[^/]+/?$")
    if d == nil or d == "" then return "/" end
    return d
end

local function tile_h()
    return s.view == "grid" and GRID_H or LINES_H
end

local function cols()
    return s.view == "grid" and s.cols or 1
end

-- ── the folder, as rows of tiles: only the rows near what is seen ──

local function cell(i, e)
    local th = s.thumbs[e.path]
    return {
        at = i - 1, name = e.name, path = e.path, kind = e.kind, video = e.kind == "video", dir = e.dir,
        thumb = th or "", has = nil, ["has.thumb"] = th ~= nil, icon = e.icon or "text-x-generic",
        picked = s.picked[e.path] == true, size = e.size_text or "", date = e.date_text or "", hidden = e.hidden == true,
    }
end

local function show_rows()
    local ROW_WINDOW = s.view == "grid" and ROWS_GRID or ROWS_LINES
    local c = cols()
    local n = #s.items
    local total = math.ceil(n / c)
    fact.total = total
    local first = math.max(0, math.min(s.first, math.max(total - ROW_WINDOW, 0)))
    local rows = {}
    for r = 1, ROW_WINDOW do
        local row = first + r - 1
        if row >= total then break end
        local cells = {}
        for k = 1, c do
            local i = row * c + k
            if i > n then break end
            cells[k] = cell(i, s.items[i])
        end
        rows[r] = { cells = cells }
    end
    fact.first = first
    model.rows = rows
end

local function refresh_soon()
    if s.refresh_due then return end
    s.refresh_due = true
    after(70, function()
        s.refresh_due = false
        if s.mode == "browse" then show_rows() else
            -- results: their pictures
            local hits = model.hits
            local changed = false
            for _, h in ipairs(s.hits) do
                local th = s.thumbs[h.path]
                if th and h._thumb ~= th then h._thumb = th; changed = true end
            end
            if changed then
                local list = {}
                for i, h in ipairs(s.hits) do list[i] = s.hit_row(i, h) end
                model.hits = list
            end
        end
    end)
end

-- ── the look on the right ──

local function fmt_time(t)
    t = math.floor(t or 0)
    local h = math.floor(t / 3600)
    local m = math.floor((t % 3600) / 60)
    local sec = t % 60
    if h > 0 then return string.format("%d:%02d:%02d", h, m, sec) end
    return string.format("%d:%02d", m, sec)
end

local function place_of(h)
    local loc = h.loc
    if loc == nil or loc == json.null then return "" end
    if h.segment == "page" or h.segment == "page_image" then return tr("page") .. " " .. math.floor(loc) end
    if h.segment == "audio" or h.segment == "frame" then return tr("at") .. " " .. fmt_time(loc) end
    if h.segment == "lines" then return tr("line") .. " " .. math.floor(loc) end
    return ""
end

-- «…the «words» that matched…» as markdown: the words in bold.
-- Marks of markdown in what a file says are shown as they are (escaped).
local function escaped(x)
    x = x:gsub("([%*_`%[%]\\#])", "\\%1")
    return x
end

local function bolded(x)
    if not x or x == json.null then return "" end
    x = escaped(x)
    x = x:gsub("«", "**"):gsub("»", "**")
    return x
end

local function mark_terms(body, terms)
    if not body or body == "" then return "" end
    body = escaped(body)
    if body:len() > 6000 then body = body:sub(1, 6000) .. "…" end
    for _, t in ipairs(terms or {}) do
        if t:len() >= 3 then
            -- case-blind, ASCII letters only (enough for the eye)
            local pat = t:gsub("%a", function(ch) return "[" .. ch:lower() .. ch:upper() .. "]" end)
            body = body:gsub("(" .. pat .. ")", "**%1**")
        end
    end
    return body
end

-- ── the player in the look ──
local player = nil
local function stop_player()
    if player then kill(player); player = nil end
    fact.playing = false
end

local function play_here(e, at)
    stop_player()
    local socket = text["win.socket"]
    if not socket or socket == "" then return false end
    local args = { "-loglevel", "error", "-noborder", "-autoexit", "-window_title", "Sonda", "-x", "640", "-y", "400" }
    if at and at > 0 then
        args[#args + 1] = "-ss"
        args[#args + 1] = tostring(math.max(at - 1, 0))
    end
    if e.kind == "audio" then
        args[#args + 1] = "-showmode"
        args[#args + 1] = "waves"
    end
    args[#args + 1] = e.path
    player = spawn("ffplay", args, function(line) log("ffplay", line) end, function(_, code)
        log("ffplay ended", code)
        player = nil
        fact.playing = false
    end, { env = { WAYLAND_DISPLAY = socket, SDL_VIDEODRIVER = "wayland" }, errors = true })
    fact.playing = player ~= nil
    return player ~= nil
end

local function look_at(entry, hit)
    stop_player()
    s.token = s.token + 1
    if not entry then
        fact["look.has"] = false
        return
    end
    fact["look.has"] = true
    fact["look.dir"] = entry.dir == true
    text["look.name"] = entry.name or basename(entry.path)
    text["look.kindname"] = tr(KIND_NAME[entry.kind] or "File")
    text["look.folder"] = dirname(entry.path):gsub("^" .. s.home:gsub("%p", "%%%0"), "~")
    text["look.path"] = entry.path
    local meta = tr(KIND_NAME[entry.kind] or "File") .. " · " .. (entry.size_text or "") .. " · " .. (entry.date_text or "")
    if hit and hit.info and hit.info ~= json.null then
        local info = hit.info
        if info.width and info.height then meta = meta .. " · " .. info.width .. "×" .. info.height end
        if info.pages then meta = meta .. " · " .. info.pages .. " " .. tr("pages") end
        if info.duration then meta = meta .. " · " .. fmt_time(info.duration) end
    end
    text["look.meta"] = meta
    local place = hit and place_of(hit) or ""
    text["look.place"] = place ~= "" and (tr("Found") .. " " .. place) or ""
    fact["look.loc"] = place ~= ""
    fact["look.media"] = entry.kind == "audio" or entry.kind == "video"
    fact["look.pdf"] = entry.kind == "pdf"
    fact["look.code"] = entry.kind == "code"
    fact["look.action"] = (entry.kind == "audio" or entry.kind == "video") and 2 or (entry.kind == "pdf" and 1 or 0)
    fact["look.image"] = false
    fact["look.text"] = false
    text["look.body"] = ""
    if hit and hit.excerpt and hit.excerpt ~= json.null then
        text["look.body"] = bolded(hit.excerpt)
        fact["look.text"] = true
    end
    local loc = hit and hit.loc
    if loc == json.null then loc = nil end
    send({ op = "preview", path = entry.path, kind = entry.kind, loc = loc, segment = hit and hit.segment or "", token = s.token })
    s.look_entry = entry
    s.look_hit = hit
end

-- ── choosing ──

local function picked_list()
    local list = {}
    for p, on in pairs(s.picked) do if on then list[#list + 1] = p end end
    table.sort(list)
    return list
end

local function count_picked()
    local n = 0
    for _, on in pairs(s.picked) do if on then n = n + 1 end end
    fact["chosen.count"] = n
end

local function choose(i, how)
    local list = s.mode == "browse" and s.items or s.hits
    local e = list[i]
    if not e then
        s.picked = {}
        s.chosen = -1
        fact.chosen = -1
        count_picked()
        if s.update_pick then s.update_pick() end
        if s.mode == "browse" then show_rows() end
        return
    end
    if how == "add" then
        s.picked[e.path] = not s.picked[e.path] or nil
    elseif how == "run" and s.anchor then
        s.picked = {}
        local a, b = math.min(s.anchor, i), math.max(s.anchor, i)
        for k = a, b do s.picked[list[k].path] = true end
    else
        s.picked = { [e.path] = true }
        s.anchor = i
    end
    s.chosen = i
    fact.chosen = i - 1
    count_picked()
    if s.pick and s.pick.mode == "save" and not e.dir then text.pickname = e.name or basename(e.path) end
    if s.update_pick then s.update_pick() end
    if s.mode == "browse" then
        show_rows()
        look_at(e, nil)
    else
        local rows = {}
        for k, h in ipairs(s.hits) do rows[k] = s.hit_row(k, h) end
        model.hits = rows
        look_at(e, e)
    end
end

-- Keeps the chosen one in sight.
local function scroll_to(i)
    local row = math.floor((i - 1) / cols())
    if row < s.first + 1 or row > s.first + 4 then
        s.first = math.max(row - 2, 0)
        show_rows()
    end
end

-- ── going places ──

local function list(path, keep_history)
    -- Which way it goes, for the scene: in (deeper), out (up), or elsewhere.
    local old = s.path
    if path ~= old then
        if old == "" or path:sub(1, #old + 1) == old .. "/" or (old == "/" and path ~= "/") then s.navdir = 1
        elseif old:sub(1, #path + 1) == path .. "/" or path == "/" then s.navdir = -1
        else s.navdir = 0.6 end
    end
    if s.path ~= "" and path ~= s.path and not keep_history then
        s.back[#s.back + 1] = s.path
        s.fwd = {}
    end
    s.path = path
    fact.loading = true
    send({ op = "list", path = path, hidden = s.hidden, sort = s.sort, filter = s.pick_filter_now and s.pick_filter_now(), dirs_only = s.pick and s.pick.mode == "folder" or nil })
end

local function go(path)
    if s.mode == "search" then
        s.mode = "browse"
        fact.mode = "browse"
    end
    list(path)
end

local function relist()
    send({ op = "list", path = s.path, hidden = s.hidden, sort = s.sort, keep = true, filter = s.pick_filter_now and s.pick_filter_now(), dirs_only = s.pick and s.pick.mode == "folder" or nil })
end

-- ── searching ──

function s.hit_row(i, h)
    local th = s.thumbs[h.path]
    local strength = math.min((h.score or 0) / math.max(s.top_score or 1, 0.001), 1)
    return {
        name = h.name, path = h.path, kind = h.kind, place = place_of(h), folder = h.folder or "",
        excerpt = (bolded(h.excerpt):gsub("%s+", " ")), thumb = th or "", ["has.thumb"] = th ~= nil, icon = h.icon or "text-x-generic",
        picked = s.picked[h.path] == true, strength = strength, date = h.date_text or "",
    }
end

local function search(q)
    q = q or text.query or ""
    if q:match("^%s*$") then return end
    s.lastq = q
    s.searchgen = s.searchgen + 1
    fact.searching = true
    if s.mode ~= "search" then
        s.mode = "search"
        fact.mode = "search"
        s.picked = {}
        fact.chosen = -1
        look_at(nil)
    end
    fact["code.mode"] = q:match("^%s*code:") ~= nil or q:match("^%s*código:") ~= nil
    local kinds = nil
    local f = FILTERS[s.filter]
    if f == "code" and not fact["code.mode"] then q = "code: " .. q end
    if f ~= "all" and f ~= "code" then
        kinds = f == "image" and { "image", "screenshot" } or { f }
    end
    send({ op = "search", q = q, kinds = kinds, limit = 60, under = s.scope == "here" and s.path or nil, id = s.searchgen, filter = s.pick_filter_now and s.pick_filter_now() })
end

local function clear_search()
    s.mode = "browse"
    fact.mode = "browse"
    fact.searched = false
    fact.searching = false
    text.query = ""
    s.hits = {}
    model.hits = {}
    s.picked = {}
    fact.chosen = -1
    look_at(nil)
    show_rows()
end

-- ── what the bridge says ──

local function on_line(line)
    local ok, m = pcall(json.decode, line)
    if not ok or type(m) ~= "table" then return end
    local t = m.t
    if t == "hello" then
        send({ op = "places" })
        send({ op = "status" })
        local start = sys.ask("env", "SONDA_START")
        if start == nil or start == "" then start = nil end
        s.start = start
        -- A file to show chosen («Show in folder»), maybe with its properties.
        local select = sys.ask("env", "SONDA_SELECT")
        if select and select ~= "" then
            s.picked = { [select] = true }
            if sys.ask("env", "SONDA_PROPS") == "1" then s.props_on_load = select end
        end
        -- Picking for another program.
        local spec = sys.ask("env", "SONDA_PICK")
        if spec and spec ~= "" then
            local ok, v = pcall(json.decode, spec)
            if ok and type(v) == "table" then s.start_pick(v) end
        end
        if sys.ask("env", "SONDA_SEARCH") == "1" then
            after(200, function() focus("query"); s.editing = true end)
        else
            after(200, function() if not s.editing then focus() end end)
        end
    elseif t == "places" then
        s.home = m.home or ""
        s.trash = m.trash or ""
        s.places = m.items or {}
        local rows = {}
        for i, p in ipairs(s.places) do
            rows[i] = { name = p.name, path = p.path, icon = PLACE_ICONS[p.icon] or "folder", here = p.path == s.path }
        end
        model.places = rows
        if s.path == "" then list(s.start or s.home) end
    elseif t == "list" then
        if m.path ~= s.path then return end
        fact.loading = false
        s.items = m.items or {}
        if not m.keep then s.first = 0 end
        if not m.keep and s.navdir then
            fact["nav.dir"] = s.navdir
            fact["nav.gen"] = fact["nav.gen"] + 1
            s.navdir = nil
        end
        -- what was picked and is still there stays picked
        local still = {}
        for _, e in ipairs(s.items) do if s.picked[e.path] then still[e.path] = true end end
        s.picked = still
        count_picked()
        fact.count = #s.items
        fact["in.trash"] = m.trash == true
        fact["can.back"] = #s.back > 0
        fact["can.forward"] = #s.fwd > 0
        fact["can.up"] = m.parent ~= nil and m.parent ~= "" and m.path ~= "/"
        s.parent = m.parent
        local crumbs = {}
        for i, c in ipairs(m.crumbs or {}) do crumbs[i] = { name = c.name, path = c.path, at = i - 1 } end
        s.crumbs = m.crumbs or {}
        model.crumbs = crumbs
        text["folder.name"] = basename(m.path)
        local dirs, files = 0, 0
        for _, e in ipairs(s.items) do if e.dir then dirs = dirs + 1 else files = files + 1 end end
        text["count.line"] = string.format("%d %s · %d %s", dirs, tr(dirs == 1 and "folder" or "folders"), files, tr(files == 1 and "file" or "files")) .. ((m.free and m.free ~= "") and (" · " .. m.free .. " " .. tr("free")) or "")
        s.writable = m.writable ~= false
        if s.show_drives then s.show_drives() end
        local rows = {}
        for i, p in ipairs(s.places) do
            rows[i] = { name = p.name, path = p.path, icon = PLACE_ICONS[p.icon] or "folder", here = p.path == s.path }
        end
        model.places = rows
        show_rows()
        if s.mode == "browse" then
            local still_chosen = nil
            for i, e in ipairs(s.items) do if s.picked[e.path] then still_chosen = i end end
            if still_chosen then
                s.chosen = still_chosen
                fact.chosen = still_chosen - 1
                s.anchor = still_chosen
                look_at(s.items[still_chosen], nil)
                -- Brought into sight (a file shown in its folder from a search).
                local row = math.floor((still_chosen - 1) / cols())
                -- The rows held start a little above it only when it lies past
                -- what they reach from the top; otherwise the first rows, which
                -- are in sight, would be left out.
                local held = s.view == "grid" and ROWS_GRID or ROWS_LINES
                if row > 2 then
                    if row >= held - 2 then s.first = math.max(row - 2, 0) end
                    show_rows()
                    s.scroll_to_row = row
                    -- (a moment later: the scene must know how long the folder is first)
                    local to = row * tile_h()
                    after(120, function()
                        fact["grid.to"] = to
                        fact["grid.go"] = fact["grid.go"] + 1
                    end)
                end
            else
                look_at(nil)
            end
        end
        if s.update_pick then s.update_pick() end
        if s.props_on_load then
            send({ op = "props", path = s.props_on_load })
            s.props_on_load = nil
        end
    elseif t == "thumb" then
        s.thumbs[m.path] = m.thumb
        refresh_soon()
    elseif t == "results" then
        if m.id ~= s.searchgen then return end
        fact.searching = false
        fact.searched = true
        fact["found.gen"] = fact["found.gen"] + 1
        s.hits = m.hits or {}
        s.top_score = (s.hits[1] and s.hits[1].score) or 1
        fact["found.n"] = #s.hits
        local rows = {}
        for i, h in ipairs(s.hits) do rows[i] = s.hit_row(i, h) end
        model.hits = rows
        if #s.hits > 0 then choose(1) else look_at(nil) end
    elseif t == "preview" then
        if m.token ~= s.token then return end
        if m.image and m.image ~= json.null then
            text["look.picture"] = m.image
            fact["look.image"] = true
        end
        if m.text and m.text ~= json.null and m.text ~= "" then
            local terms = s.look_hit and s.look_hit.terms or {}
            if s.look_hit and s.look_hit.excerpt and s.look_hit.excerpt ~= json.null and s.look_entry and s.look_entry.kind ~= "code" then
                -- the stretch found, and then the rest of that page or passage
                text["look.body"] = mark_terms(m.text, terms)
            else
                text["look.body"] = mark_terms(m.text, terms)
            end
            fact["look.text"] = true
        end
        if m.folder_size and m.folder_size ~= json.null then
            text["look.meta"] = tr("Folder") .. " · " .. m.folder_size .. " · " .. (s.look_entry and s.look_entry.size_text or "")
        end
    elseif t == "done" then
        if m.job then
            local was = s.jobs[m.job]
            if was and was.shown and m.ok then fact["jobs.splash"] = fact["jobs.splash"] + 1 end
            s.jobs[m.job] = nil
            s.show_jobs()
        end
        if m.op == "mount" or m.op == "eject" or m.op == "unmount" then
            s.mounting[m.dev or ""] = nil
            s.show_drives()
        end
        if m.cancelled then
            toast(tr("Stopped"))
        elseif not m.ok then
            if m.op == "undo" and tostring(m.error):find("nothing to undo") then
                toast(tr("Nothing to undo"))
            else
                toast(tr("Could not") .. ": " .. tostring(m.error))
            end
        else
            local msg = ({
                trash = tr("Moved to the trash"), restore = tr("Restored"), empty_trash = tr("The trash is empty"),
                copy = tr("Copied"), move = tr("Moved"), rename = tr("Renamed"), mkdir = tr("Folder made"),
                clip = tr("Ready to paste"), drop = tr("Copied here"), compress = tr("Packed"), extract = tr("Unpacked"),
                eject = tr("You can take it out now"), chmod = tr("Permissions changed"),
            })[m.op]
            if m.op == "undo" then msg = tr("Undone") .. ": " .. tr(UNDONE[m.what] or "what was done") end
            if msg then toast(msg) end
            if m.op == "rename" or m.op == "mkdir" or m.op == "compress" or m.op == "extract" or ((m.op == "copy" or m.op == "move") and m.made and #m.made > 0) then
                s.picked = {}
                for _, p in ipairs(m.made or {}) do s.picked[p] = true end
            end
            if m.op == "mount" and m.path and m.path ~= "" then go(m.path) end
        end
        if m.op ~= "open" and m.op ~= "clip" and m.op ~= "open_with" and m.op ~= "terminal" and m.op ~= "mount" and m.op ~= "props" then
            if s.mode == "browse" then relist() else search(s.lastq) end
        end
    elseif t == "job" then
        local j = s.jobs[m.id] or { shown = false }
        s.jobs[m.id] = j
        j.title = (({ copy = tr("Copying to"), move = tr("Moving to"), compress = tr("Packing"), extract = tr("Unpacking") })[m.op] or "") .. " " .. (m.dest or "")
        if m.op == "copy" or m.op == "move" then
            j.line = string.format("%s %s %s", m.done_text or "", tr("of"), m.total_text or "") .. ((m.file and m.file ~= "") and (" · " .. m.file) or "")
        else
            j.line = string.format("%d %s %d", m.done or 0, tr("of"), m.total or 0) .. ((m.file and m.file ~= "") and (" · " .. m.file) or "")
        end
        j.frac = (m.done or 0) / math.max(m.total or 1, 1)
        if not j.timer then
            -- Only what takes a while gets a card: a small copy is just done.
            j.timer = true
            after(400, function() if s.jobs[m.id] then s.jobs[m.id].shown = true; s.show_jobs() end end)
        end
        if j.shown then s.show_jobs() end
    elseif t == "conflict" then
        s.cf = { kind = "job", job = m.job }
        local names = {}
        for i, n in ipairs(m.names or {}) do names[i] = "«" .. n .. "»" end
        local more = (m.count or 0) - #names
        text["cf.names"] = table.concat(names, ", ") .. (more > 0 and string.format(" %s %d %s", tr("and"), more, tr("more")) or "")
        if (m.count or 0) == 1 then
            text["cf.title"] = string.format(tr("There is already one with that name in %s"), m.dest or "")
        else
            text["cf.title"] = string.format(tr("%d of them are already in %s"), m.count or 0, m.dest or "")
        end
        fact["cf.many"] = (m.count or 0) > 1
        fact["cf.save"] = false
        fact["cf.open"] = true
    elseif t == "drives" then
        s.drives = m.items or {}
        s.show_drives()
    elseif t == "apps" then
        s.show_apps(m)
    elseif t == "props" then
        s.show_props(m)
    elseif t == "props_size" then
        if m.path == s.props_path then
            text["pr.size"] = string.format("%s · %d %s, %d %s", m.size_text or "", m.files or 0, tr((m.files or 0) == 1 and "file" or "files"), m.dirs or 0, tr((m.dirs or 0) == 1 and "folder" or "folders"))
        end
    elseif t == "exists" then
        if s.on_exists then s.on_exists(m) end
    elseif t == "status" then
        if m.ok == false then
            fact["read.reachable"] = false
            text["status.line"] = tr("The reader is not running")
            return
        end
        fact["read.reachable"] = true
        local st = m.status or {}
        if m.config then
            cfg = m.config
            if fact["settings.open"] and s.show_folders then s.show_folders() end
        end
        fact["read.done"] = st.done or 0
        fact["read.pending"] = st.pending or 0
        fact["read.errors"] = st.errors or 0
        fact["read.rate"] = st.per_minute or 0
        fact["read.vectors"] = st.vectors or 0
        fact["read.paused"] = (cfg.paused == true) or st.phase == "paused"
        fact["read.on"] = st.phase == "reading"
        local done, pending = st.done or 0, st.pending or 0
        if st.phase == "paused" or cfg.paused then
            text["status.line"] = string.format("%s · %d / %d", tr("Paused"), done, done + pending)
        elseif st.phase == "waiting" then
            text["status.line"] = string.format("%s · %d / %d", tr("A video is playing"), done, done + pending)
        elseif pending > 0 then
            text["status.line"] = string.format("%d %s %d · %d/min", done, tr("of"), done + pending, math.floor(st.per_minute or 0))
        elseif st.phase == "walking" or st.phase == "starting" then
            text["status.line"] = tr("Looking at the folders…")
        else
            text["status.line"] = string.format("%s · %d %s", tr("All read"), done, tr("files"))
        end
        text["status.file"] = (st.current and st.current ~= "") and basename(st.current) or string.format("%d %s", st.vectors or 0, tr("pieces in the index"))
    elseif t == "error" then
        if m.op == "list" then
            fact.loading = false
            toast(tr("Cannot open") .. " " .. basename(m.path or ""))
            if #s.back > 0 then s.path = table.remove(s.back) end
        elseif m.op == "search" then
            fact.searching = false
            toast(tr("The search failed") .. ": " .. tostring(m.error))
        end
    end
end

local function start_bridge()
    bridge = spawn("sonda", { "bridge" }, on_line, function(_, code)
        bridge = nil
        log("sonda bridge ended", code)
        after(1500, start_bridge)
    end, { stdin = "open" })
end
start_bridge()
-- The scene says how many columns fit as soon as it knows the logic is here.
fact["ui.ready"] = true

every(2000, function() send({ op = "status" }) end)

-- ── the scene's events ──

on("go", function(k)
    local p = s.places[k + 1]
    if p then go(p.path) end
end)

on("crumb", function(k)
    local c = s.crumbs[k + 1]
    if c then go(c.path) end
end)

on("nav", function(k)
    if k == 0 and #s.back > 0 then
        s.fwd[#s.fwd + 1] = s.path
        if s.mode == "search" then clear_search() end
        list(table.remove(s.back), true)
    elseif k == 1 and #s.fwd > 0 then
        s.back[#s.back + 1] = s.path
        list(table.remove(s.fwd), true)
    elseif k == 2 and s.parent and s.parent ~= "" then
        go(s.parent)
    elseif k == 3 then
        relist()
    end
end)

local function open_entry(e, hit)
    if e.dir then
        go(e.path)
        return
    end
    if s.pick then
        if s.pick.mode == "open" then s.finish_pick({ e.path })
        elseif s.pick.mode == "save" then text.pickname = e.name or basename(e.path); s.accept_pick() end
        return
    end
    local loc = hit and hit.loc
    if loc == json.null then loc = nil end
    send({ op = "open", path = e.path, kind = e.kind, loc = loc })
end

-- Pressed: chosen; pressed again soon, opened. Ctrl adds, Shift takes the run.
local function pressed(i, list_items)
    focus()
    s.editing = false
    if s.lastpress == i and s.lastpressed then
        s.lastpressed = false
        local e = list_items[i]
        if e then open_entry(e, s.mode == "search" and e or nil) end
        return
    end
    s.lastpress = i
    s.lastpressed = true
    after(400, function() s.lastpressed = false end)
    local ctrl = held("Control_L") or held("Control_R") or held("ctrl")
    local shift = held("Shift_L") or held("Shift_R") or held("shift")
    choose(i, ctrl and "add" or (shift and "run" or nil))
end

on("tile", function(k)
    if k < 0 then
        focus()
        s.editing = false
        choose(nil)
        return
    end
    pressed(k + 1, s.items)
end)

on("hit", function(k) pressed(k + 1, s.hits) end)

-- What can be done, as the menu shows it: with what was chosen, or with the folder.
local function menu_rows(on_item)
    local rows = {}
    local function add(k, label, keys, danger, sep)
        rows[#rows + 1] = { k = k, label = tr(label), keys = keys and tr(keys) or "", danger = danger == true, sep = sep == true }
    end
    local list = picked_list()
    local n = #list
    local items = s.mode == "browse" and s.items or s.hits
    local entry = nil
    for _, e in ipairs(items) do if e.path == list[1] then entry = e end end
    local trash = fact["in.trash"]
    local picking = s.pick ~= nil
    if on_item then
        if trash then
            add(10, "Restore")
            add(15, "Properties", "Alt+Enter")
        else
            add(0, "Open", "Enter")
            if fact["look.loc"] and s.mode == "search" then add(1, "Open where it was found") end
            if n == 1 and entry and not entry.dir and not picking then add(12, "Open with…") end
            if s.mode == "search" then add(2, "Show in its folder") end
            add(3, "Copy", "Ctrl+C")
            add(4, "Cut", "Ctrl+X")
            if s.clip and s.mode == "browse" then add(5, "Paste", "Ctrl+V") end
            if n == 1 then add(6, "Rename…", "F2") end
            add(8, "Copy the path")
            if not picking then
                if n == 1 and entry and entry.kind == "archive" then add(14, "Extract here") end
                add(13, "Compress…")
            end
            if n == 1 then add(15, "Properties", "Alt+Enter") end
            if not picking then add(9, "Move to the trash", "Delete", true, true) end
        end
    else
        if trash then
            add(11, "Empty the trash", nil, true, false)
        else
            if s.clip and s.mode == "browse" then add(5, "Paste", "Ctrl+V") end
            if s.mode == "browse" then add(7, "New folder…", "Ctrl+Shift+N") end
            add(17, "Undo", "Ctrl+Z")
            if not picking then add(16, "Open a terminal here") end
            add(15, "Properties", "Alt+Enter")
        end
    end
    return rows
end

local function open_menu(on_item)
    fact["menu.on.item"] = on_item
    model.menu = menu_rows(on_item)
    fact["menu.open"] = true
end

on("tile_menu", function(k)
    local e = s.items[k + 1]
    if e and not s.picked[e.path] then choose(k + 1) end
    open_menu(true)
end)

on("hit_menu", function(k)
    local h = s.hits[k + 1]
    if h and not s.picked[h.path] then choose(k + 1) end
    open_menu(true)
end)

on("bg_menu", function()
    choose(nil)
    open_menu(false)
end)

local function ask(kind, title, value)
    s.asking = kind
    fact["ask.kind"] = kind
    text["ask.title"] = title
    text.answer = value or ""
    fact["ask.open"] = true
    after(60, function() focus("answer") end)
end

on("act", function(k)
    local list = picked_list()
    local first = list[1]
    local entry = nil
    local items = s.mode == "browse" and s.items or s.hits
    for _, e in ipairs(items) do if e.path == first then entry = e end end
    if k == 0 then
        if entry then open_entry(entry, s.mode == "search" and entry or nil) end
    elseif k == 1 then
        if entry then open_entry(entry, entry) end
    elseif k == 2 then
        if first then
            local dir = dirname(first)
            clear_search()
            s.picked = { [first] = true }
            go(dir)
        end
    elseif k == 3 or k == 4 then
        if #list > 0 then
            s.clip = { paths = list, cut = k == 4 }
            fact["clip.count"] = #list
            fact["clip.cut"] = k == 4
            send({ op = "clip", paths = list, cut = k == 4 })
        end
    elseif k == 5 then
        if s.clip and s.mode == "browse" then
            send({ op = s.clip.cut and "move" or "copy", paths = s.clip.paths, dest = s.path })
            if s.clip.cut then s.clip = nil; fact["clip.count"] = 0 end
        end
    elseif k == 6 then
        if first and not fact["in.trash"] then ask("rename", tr("Rename"), basename(first)) end
    elseif k == 7 then
        if s.mode == "browse" and not fact["in.trash"] then ask("folder", tr("New folder"), tr("New folder")) end
    elseif k == 8 then
        if #list > 0 then run("wl-copy", { table.concat(list, "\n") }); toast(tr("Path copied")) end
    elseif k == 9 then
        if #list > 0 and not fact["in.trash"] then send({ op = "trash", paths = list }) end
    elseif k == 10 then
        if #list > 0 then
            local names = {}
            for i, p in ipairs(list) do names[i] = basename(p) end
            send({ op = "restore", names = names })
        end
    elseif k == 11 then
        send({ op = "empty_trash" })
    elseif k == 12 then
        if first then
            s.ow_paths = { first }
            send({ op = "apps", path = first })
        end
    elseif k == 13 then
        if #list > 0 and s.mode == "browse" and not fact["in.trash"] then
            s.compress_paths = list
            local name = #list == 1 and (basename(first):gsub("%.[^%.]+$", "")) or basename(s.path)
            ask("compress", tr("Compress"), name .. ".zip")
        end
    elseif k == 14 then
        if entry and entry.kind == "archive" then send({ op = "extract", path = first }) end
    elseif k == 15 then
        send({ op = "props", path = first or s.path })
    elseif k == 16 then
        send({ op = "terminal", path = s.path })
    elseif k == 17 then
        send({ op = "undo" })
    end
end)

on("answered", function()
    local v = text.answer or ""
    fact["ask.open"] = false
    focus()
    if v:match("^%s*$") then return end
    if s.asking == "rename" then
        local first = picked_list()[1]
        if first then send({ op = "rename", path = first, name = v }) end
    elseif s.asking == "compress" then
        send({ op = "compress", paths = s.compress_paths or picked_list(), name = v })
    else
        send({ op = "mkdir", dir = s.path, name = v })
    end
end)

on("ask_cancel", function()
    fact["ask.open"] = false
    focus()
end)

on("look_act", function(k)
    local e = s.look_entry
    if not e then return end
    if k == 0 then open_entry(e, nil)
    elseif k == 1 then
        local media = e.kind == "audio" or e.kind == "video"
        local at = s.look_hit and s.look_hit.loc
        if at == json.null then at = nil end
        if not (media and play_here(e, at)) then open_entry(e, s.look_hit) end
    elseif k == 2 then
        local dir = e.dir and e.path or dirname(e.path)
        if s.mode == "search" then clear_search() end
        s.picked = { [e.path] = true }
        go(e.dir and dirname(e.path) or dir)
    elseif k == 3 then
        run("wl-copy", { e.path })
        toast(tr("Path copied"))
    end
end)

on("slid", function(scroll)
    local first = math.max(math.floor(scroll / tile_h()) - 2, 0)
    if math.abs(first - s.first) >= 2 then
        s.first = first
        show_rows()
    end
end)

on("set_cols", function(n)
    n = math.max(1, math.min(math.floor(n), MAX_COLS))
    if n ~= s.cols then
        s.cols = n
        fact.cols = n
        show_rows()
    end
end)

on("set_view", function(v)
    s.view = v == 1 and "lines" or "grid"
    fact.view = s.view
    s.first = 0
    show_rows()
end)

on("set_sort", function()
    local order = { name = "date", date = "size", size = "kind", kind = "name" }
    s.sort = order[s.sort] or "name"
    fact.sort = s.sort
    relist()
end)

on("toggle_hidden", function()
    s.hidden = not s.hidden
    fact.hidden = s.hidden
    relist()
end)

on("toggle_preview", function()
    fact["preview.on"] = not fact["preview.on"]
end)

on("set_scope", function(v)
    s.scope = v == 1 and "here" or "everywhere"
    fact.scope = s.scope
    if s.mode == "search" then search(s.lastq) end
end)

on("set_filter", function(k)
    s.filter = k + 1
    fact.filter = FILTERS[k + 1]
    if s.mode == "search" then search(s.lastq) end
end)

on("pause_toggle", function()
    send({ op = fact["read.paused"] and "resume" or "pause" })
    fact["read.paused"] = not fact["read.paused"]
end)

-- ── which folders it reads ──
local function show_folders()
    local rf, cf = {}, {}
    for i, p in ipairs(cfg.folders or {}) do rf[i] = { name = basename(p), path = p:gsub("^" .. s.home:gsub("%p", "%%%0"), "~") } end
    for i, p in ipairs(cfg.code_folders or {}) do cf[i] = { name = basename(p), path = p:gsub("^" .. s.home:gsub("%p", "%%%0"), "~") } end
    model.readf = rf
    model.codef = cf
    local inside = false
    for _, p in ipairs(cfg.folders or {}) do if s.path == p or s.path:sub(1, #p + 1) == p .. "/" then inside = true end end
    for _, p in ipairs(cfg.code_folders or {}) do if s.path == p or s.path:sub(1, #p + 1) == p .. "/" then inside = true end end
    fact["here.read"] = inside or s.path == "/" or s.path == s.trash
end
s.show_folders = show_folders

on("settings_open", function()
    show_folders()
    fact["settings.open"] = true
end)

on("settings_act", function(k)
    local f, c = {}, {}
    for i, p in ipairs(cfg.folders or {}) do f[i] = p end
    for i, p in ipairs(cfg.code_folders or {}) do c[i] = p end
    if k == 0 then f[#f + 1] = s.path; toast(tr("Now it reads") .. " " .. basename(s.path))
    elseif k == 1 then c[#c + 1] = s.path; toast(tr("Now it reads") .. " " .. basename(s.path))
    elseif k >= 200 then local gone = table.remove(c, k - 200 + 1); if gone then toast(tr("It no longer reads") .. " " .. basename(gone)) end
    elseif k >= 100 then local gone = table.remove(f, k - 100 + 1); if gone then toast(tr("It no longer reads") .. " " .. basename(gone)) end
    end
    cfg.folders, cfg.code_folders = f, c
    show_folders()
    send({ op = "folders", folders = f, code_folders = c })
end)

on("clear_search", clear_search)
on("stop_player", stop_player)
on("search_now", function() search(text.query) end)

-- Typing: a search a moment after the last key.
local typed_at = 0
local function query_changed(v)
    s.editing = true
    typed_at = typed_at + 1
    local mine = typed_at
    if v:match("^%s*$") then
        if s.mode == "search" then clear_search() end
        return
    end
    after(380, function()
        if mine == typed_at and v ~= s.lastq then search(v) end
    end)
end
on("text:query", query_changed)

-- Something dropped: files from another program (or from here) into a
-- folder. The data comes by the zone's name, which folder by the scene's
-- event: whichever is second does it.
local drop_data, drop_at = nil, nil
local function do_drop()
    if not drop_data or not drop_at then return end
    local data, k = drop_data, drop_at
    drop_data, drop_at = nil, nil
    local dest = s.path
    if k >= 0 then
        local e = s.items[k + 1]
        if e and e.dir then dest = e.path end
    elseif k <= -300 then
        local d = s.drives[-300 - k + 1]
        if not d or d.mount == "" then toast(tr("Open the drive first")); return end
        dest = d.mount
    elseif k <= -200 then
        local c = s.crumbs[-200 - k + 1]
        if c then dest = c.path end
    elseif k <= -100 then
        local p = s.places[-100 - k + 1]
        if p then dest = p.path end
    end
    -- Carried from this window (from the folder shown), it moves; from
    -- another program, it is copied.
    local inside = true
    for line in data:gmatch("[^\r\n]+") do
        local p = line:gsub("^file://", "")
        if dirname(p) ~= s.path then inside = false end
    end
    if dest == s.path and inside then return end
    if dest == s.trash then
        local paths = {}
        for line in data:gmatch("[^\r\n]+") do paths[#paths + 1] = (line:gsub("^file://", "")) end
        send({ op = "trash", paths = paths })
        return
    end
    send({ op = "drop", data = data, dest = dest, move = inside })
end
for _, zone in ipairs({ "midzone", "t", "place", "cb" }) do
    on("drop:" .. zone, function(data)
        drop_data = data
        do_drop()
    end)
end
on("dropped", function(k)
    drop_at = k
    do_drop()
end)

-- Keys that move the choice: arrows, Enter, Backspace, Space, Home, End, Ctrl+A.
on("key_nav", function(k)
    local items = s.mode == "browse" and s.items or s.hits
    local n = #items
    local c = s.mode == "browse" and cols() or 1
    local i = s.chosen
    if k == 9 then
        if s.mode == "browse" then
            s.picked = {}
            for _, e in ipairs(s.items) do s.picked[e.path] = true end
            count_picked()
            show_rows()
        end
        return
    end
    if k == 5 then
        if s.mode == "browse" and not s.editing then
            if #s.back > 0 then
                s.fwd[#s.fwd + 1] = s.path
                list(table.remove(s.back), true)
            end
        end
        return
    end
    if k == 4 then
        if items[i] then open_entry(items[i], s.mode == "search" and items[i] or nil) end
        return
    end
    if k == 6 then
        fact["preview.on"] = not fact["preview.on"]
        return
    end
    if n == 0 then return end
    if i < 1 then i = 0 end
    if k == 0 then i = math.max(i - 1, 1)
    elseif k == 1 then i = math.min(i + 1, n)
    elseif k == 2 then i = math.max(i - c, 1)
    elseif k == 3 then i = math.min(i + c, n); if i < 1 then i = 1 end
    elseif k == 7 then i = 1
    elseif k == 8 then i = n
    end
    if s.editing and (k == 0 or k == 1) then return end
    choose(i)
    if s.mode == "browse" then scroll_to(i) end
end)

-- Letters typed on the folder go to the search.
on("key", function(name, typed)
    if fact["ask.open"] or fact["menu.open"] or fact["cf.open"] or fact["ow.open"] or fact["pr.open"] or fact["settings.open"] then return end
    if name:find("+", 1, true) and name ~= "plus" then return end
    if typed and typed ~= "" and not s.editing and typed:match("^[%w%sáéíóúñüÁÉÍÓÚÑÜ%.%-_]$") and typed ~= " " then
        s.editing = true
        text.query = (text.query or "") .. typed
        focus("query")
        query_changed(text.query)
    end
end)

on("focus", function() end)

-- ── long jobs ──
function s.show_jobs()
    local ids = {}
    for id, j in pairs(s.jobs) do if j.shown then ids[#ids + 1] = id end end
    table.sort(ids)
    local rows = {}
    for i = 1, math.min(#ids, 4) do
        local j = s.jobs[ids[i]]
        rows[i] = { id = ids[i], title = j.title or "", line = j.line or "", frac = j.frac or 0 }
    end
    model.jobs = rows
end

on("job_cancel", function(id)
    send({ op = "cancel", job = id })
end)

on("resolve", function(k)
    fact["cf.open"] = false
    local cf = s.cf
    s.cf = nil
    if not cf then return end
    if cf.kind == "job" then
        send({ op = "resolve", job = cf.job, how = ({ "replace", "skip", "both", "cancel" })[k + 1] })
    elseif cf.kind == "save" and k == 0 then
        s.finish_pick({ cf.path })
    end
end)

on("undo", function() send({ op = "undo" }) end)

-- ── drives ──
local function under(path, root)
    return root ~= "" and (path == root or path:sub(1, #root + 1) == root .. "/")
end

function s.show_drives()
    local rows = {}
    for i, d in ipairs(s.drives) do
        rows[i] = { name = d.name, size = d.size_text or "", icon = d.icon or "drive-harddisk", mounted = (d.mount or "") ~= "", here = under(s.path, d.mount or ""), busy = s.mounting[d.dev] == true }
    end
    model.drives = rows
end

on("drive", function(k)
    local d = s.drives[k + 1]
    if not d then return end
    if (d.mount or "") ~= "" then go(d.mount); return end
    s.mounting[d.dev] = true
    s.show_drives()
    send({ op = "mount", dev = d.dev })
end)

on("eject", function(k)
    local d = s.drives[k + 1]
    if not d then return end
    if under(s.path, d.mount or "") then go(s.home) end
    s.mounting[d.dev] = true
    s.show_drives()
    send({ op = d.removable and "eject" or "unmount", dev = d.dev })
end)

-- ── open with ──
local ow_last = nil
function s.show_apps(m)
    s.apps = {}
    local rows = {}
    local had_rec, headed = false, false
    local pickk = -1
    for i, a in ipairs(m.items or {}) do
        if i > 60 then break end
        s.apps[i] = a
        local head = false
        if a.recommended then had_rec = true elseif had_rec and not headed then head = true; headed = true end
        rows[i] = { name = a.name, icon = a.icon, rec = a.recommended == true, def = a.default == true, head = head }
        if pickk < 0 and (a.default or a.recommended) then pickk = i - 1 end
    end
    model.apps = rows
    text["ow.title"] = string.format(tr("Open «%s» with"), basename(m.path or ""))
    text["ow.kind"] = m.mime or ""
    fact["ow.pick"] = pickk
    fact["ow.always"] = false
    fact["ow.open"] = true
    ow_last = nil
end

local function ow_go()
    local a = s.apps[(fact["ow.pick"] or -1) + 1]
    if not a or not fact["ow.open"] then return end
    fact["ow.open"] = false
    send({ op = "open_with", app = a.id, paths = s.ow_paths or {}, always = fact["ow.always"] == true })
    if fact["ow.always"] then toast(string.format(tr("%s opens these now"), a.name)) end
end

on("ow_pick", function(k)
    -- Twice: open with it.
    if ow_last == k and fact["ow.pick"] == k then
        ow_go()
        return
    end
    ow_last = k
    after(400, function() ow_last = nil end)
    fact["ow.pick"] = k
end)

on("ow_go", ow_go)

on("ow_cancel", function() fact["ow.open"] = false end)

-- ── properties ──
function s.show_props(m)
    s.props_path = m.path
    text["pr.name"] = m.name or ""
    text["pr.kind"] = tr(KIND_NAME[m.kind] or "File") .. " · " .. (m.mime or "")
    text["pr.size"] = m.dir and tr("Counting…") or (m.size_text or "")
    text["pr.where"] = ((m.folder or ""):gsub("^" .. s.home:gsub("%p", "%%%0"), "~"))
    text["pr.modified"] = m.modified or ""
    text["pr.accessed"] = m.accessed or ""
    text["pr.created"] = m.created or ""
    text["pr.owner"] = (m.owner or "") .. " · " .. (m.group or "")
    text["pr.linkto"] = m.link or ""
    text["pr.free"] = (m.free_text or "") .. " " .. tr("free")
    fact["pr.link"] = (m.link or "") ~= ""
    fact["pr.dir"] = m.dir == true
    fact["pr.mine"] = m.mine == true
    fact["pr.mode"] = m.mode or 0
    fact["pr.used"] = m.used or 0
    model.pricon = { { icon = s.thumbs[m.path] or m.icon or "text-x-generic" } }
    fact["pr.open"] = true
end

on("pr_bit", function(bit)
    if not s.props_path then return end
    local mode = bit32.bxor(fact["pr.mode"] or 0, bit32.lshift(1, bit))
    fact["pr.mode"] = mode
    send({ op = "chmod", path = s.props_path, mode = mode })
end)

on("pr_close", function() fact["pr.open"] = false end)

-- ── picking for another program ──
local function pick_filters()
    return (s.pick and s.pick.filters) or {}
end

function s.pick_filter_now()
    local f = pick_filters()[s.pickfilter]
    if not f then return nil end
    return { globs = f.globs or {}, mimes = f.mimes or {} }
end

local function plain(label)
    -- «_Open» (a GTK mnemonic) → «Open»
    return (label or ""):gsub("_(%w)", "%1")
end

function s.start_pick(spec)
    s.pick = spec
    fact["pick.on"] = true
    fact["pick.mode"] = spec.mode == "save" and "save" or (spec.mode == "folder" and "folder" or "open")
    s.pickfilter = (tonumber(spec.filter) or 0) + 1
    local filters = pick_filters()
    fact["pick.filters"] = #filters
    text["pick.filter"] = filters[s.pickfilter] and filters[s.pickfilter].name or ""
    local title = plain(spec.title)
    if title == "" then
        title = tr(({ open = "Open a file", save = "Save as", folder = "Choose a folder" })[fact["pick.mode"]])
    end
    if spec.app and spec.app ~= "" then title = spec.app .. " · " .. title end
    text["pick.title"] = title
    local accept = plain(spec.accept)
    if accept == "" then accept = tr(({ open = "Open", save = "Save", folder = "Choose" })[fact["pick.mode"]]) end
    text["pick.accept"] = accept
    if fact["pick.mode"] == "save" then
        text.pickname = spec.name or ""
        after(300, function() focus("pickname"); s.editing = true end)
    end
    s.update_pick()
end

local function picked_entries()
    local items = s.mode == "browse" and s.items or s.hits
    local out = {}
    for _, e in ipairs(items) do if s.picked[e.path] then out[#out + 1] = e end end
    return out
end

function s.update_pick()
    if not s.pick then return end
    local mode = fact["pick.mode"]
    local chosen = picked_entries()
    if mode == "open" then
        local files, dirs = {}, {}
        for _, e in ipairs(chosen) do if e.dir then dirs[#dirs + 1] = e else files[#files + 1] = e end end
        if #files == 1 then text["pick.what"] = files[1].name
        elseif #files > 1 then text["pick.what"] = string.format("%d %s", #files, tr("files"))
        else text["pick.what"] = tr(s.pick.multiple and "Choose one or more files" or "Choose a file") end
        fact["pick.ok"] = #files > 0 or #dirs == 1
    elseif mode == "folder" then
        local target = s.path
        if #chosen == 1 then target = chosen[1].dir and chosen[1].path or dirname(chosen[1].path) end
        s.pick_target = target
        text["pick.what"] = (target:gsub("^" .. s.home:gsub("%p", "%%%0"), "~"))
        fact["pick.ok"] = target ~= "" and not fact["in.trash"]
    else
        local name = text.pickname or ""
        fact["pick.ok"] = not name:match("^%s*$") and s.writable and not fact["in.trash"]
    end
end

function s.finish_pick(paths)
    if not s.pick or s.pick_done then return end
    s.pick_done = true
    send({ op = "pick", paths = paths, filter = s.pickfilter - 1 })
end

local exists_token = 0
function s.accept_pick()
    if not s.pick then return end
    local mode = fact["pick.mode"]
    if mode == "open" then
        local files, dir = {}, nil
        for _, e in ipairs(picked_entries()) do if e.dir then dir = e else files[#files + 1] = e.path end end
        if #files == 0 then
            if dir then go(dir.path) end
            return
        end
        if not s.pick.multiple then files = { files[1] } end
        s.finish_pick(files)
    elseif mode == "folder" then
        s.update_pick()
        if s.pick_target and s.pick_target ~= "" then s.finish_pick({ s.pick_target }) end
    else
        local name = (text.pickname or ""):gsub("^%s+", ""):gsub("%s+$", "")
        if name == "" then return end
        if name:find("/", 1, true) then toast(tr("A name cannot have /")); return end
        local path = (s.path == "/" and "" or s.path) .. "/" .. name
        exists_token = exists_token + 1
        local mine = exists_token
        s.on_exists = function(m)
            if m.token ~= mine then return end
            s.on_exists = nil
            if m.exists then
                s.cf = { kind = "save", path = path }
                text["cf.title"] = string.format(tr("There is already one with that name in %s"), basename(s.path))
                text["cf.names"] = "«" .. name .. "»"
                fact["cf.many"] = false
                fact["cf.save"] = true
                fact["cf.open"] = true
            else
                s.finish_pick({ path })
            end
        end
        send({ op = "exists", path = path, token = mine })
    end
end

on("pick_accept", function() s.accept_pick() end)
on("pick_cancel", function()
    if not s.pick or s.pick_done then return end
    s.pick_done = true
    send({ op = "pick", cancel = true })
end)
on("pick_filter", function()
    local filters = pick_filters()
    if #filters < 2 then return end
    s.pickfilter = s.pickfilter % #filters + 1
    text["pick.filter"] = filters[s.pickfilter].name or ""
    if s.mode == "browse" then relist() else search(s.lastq) end
end)
on("text:pickname", function() s.editing = true; s.update_pick() end)
