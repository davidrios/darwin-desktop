-- Built-in default config, used when ~/.config/hypr/hyprland.lua doesn't exist.
-- Same API as Hyprland's Lua config (structure after Hyprland's example config,
-- BSD 3-Clause). SUPER is Command.
--
-- Differences from Hyprland's example binds, for Mac hands: Cmd+Return opens
-- the terminal and Cmd+Q closes the focused window. Cmd+Shift+Q always leaves
-- the session and can't be rebound.

-- No Wayland terminal builds for Darwin yet; GTK's widget factory stands in.
local terminal = os.getenv("TERMINAL") or "gtk4-widget-factory"

hl.config({
    general = {
        gaps_in     = 5,
        gaps_out    = 20,
        border_size = 2,
        col = {
            active_border   = { colors = { "rgba(33ccffee)", "rgba(00ff99ee)" }, angle = 45 },
            inactive_border = "rgba(595959aa)",
        },
        layout      = "dwindle",
    },
    decoration = {
        rounding = 10,
    },
    dwindle = {
        preserve_split = true,
    },
    input = {
        follow_mouse = 1,
    },
})

local mainMod = "SUPER"

hl.bind(mainMod .. " + Return", hl.dsp.exec_cmd(terminal))
hl.bind(mainMod .. " + Q", hl.dsp.window.close())
hl.bind(mainMod .. " + V", hl.dsp.window.float({ action = "toggle" }))
hl.bind(mainMod .. " + F", hl.dsp.window.fullscreen())
hl.bind(mainMod .. " + J", hl.dsp.layout("togglesplit"))

for _, dir in ipairs({ "left", "right", "up", "down" }) do
    hl.bind(mainMod .. " + " .. dir, hl.dsp.focus({ direction = dir }))
    hl.bind(mainMod .. " + SHIFT + " .. dir, hl.dsp.window.move({ direction = dir }))
end

for i = 1, 10 do
    local key = i % 10 -- 10 maps to key 0
    hl.bind(mainMod .. " + " .. key, hl.dsp.focus({ workspace = i }))
    hl.bind(mainMod .. " + SHIFT + " .. key, hl.dsp.window.move({ workspace = i }))
end

-- Cmd+Tab / Cmd+Shift+Tab: next and previous non-empty workspace.
hl.bind(mainMod .. " + Tab", hl.dsp.focus({ workspace = "e+1" }))
hl.bind(mainMod .. " + SHIFT + Tab", hl.dsp.focus({ workspace = "e-1" }))
