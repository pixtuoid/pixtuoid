use pixtuoid_core::sprite::Rgb;

use super::*;

/// Gruvbox Dark — retro warm amber on dark brown.
/// Based on <https://github.com/morhetz/gruvbox>
pub(crate) static GRUVBOX: Theme = Theme {
    name: "gruvbox",
    kind: ThemeKind::Dark,
    surface: SurfaceColors {
        wall: Rgb {
            r: 40,
            g: 40,
            b: 40,
        },
        wall_trim: Rgb {
            r: 80,
            g: 73,
            b: 69,
        },
        carpet_base: Rgb {
            r: 60,
            g: 56,
            b: 54,
        },
        carpet_light: Rgb {
            r: 80,
            g: 73,
            b: 69,
        },
        carpet_dark: Rgb {
            r: 50,
            g: 48,
            b: 47,
        },
        window_frame: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        bg_fallback: Rgb {
            r: 40,
            g: 40,
            b: 40,
        },
    },
    office: OfficeColors {
        room_wall_trim_light: Rgb {
            r: 102,
            g: 92,
            b: 84,
        },
        room_wall_trim_dark: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        runner_base: Rgb {
            r: 70,
            g: 60,
            b: 45,
        },
        runner_stripe: Rgb {
            r: 90,
            g: 75,
            b: 55,
        },
        runner_edge: Rgb {
            r: 50,
            g: 42,
            b: 32,
        },
        neon_panel_bg: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        building_dark: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        building_light: Rgb {
            r: 85,
            g: 78,
            b: 72,
        },
        city_lit_windows: [
            Rgb {
                r: 250,
                g: 189,
                b: 47,
            },
            Rgb {
                r: 254,
                g: 128,
                b: 25,
            },
            Rgb {
                r: 184,
                g: 187,
                b: 38,
            },
        ],
        city_dark_window: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        clock_rim: Rgb {
            r: 168,
            g: 153,
            b: 132,
        },
        clock_face: Rgb {
            r: 235,
            g: 219,
            b: 178,
        },
        clock_hand: Rgb {
            r: 40,
            g: 40,
            b: 40,
        },
        shadow: Rgb {
            r: 20,
            g: 20,
            b: 18,
        },
    },
    lighting: LightingColors {
        day_sky_a: Rgb {
            r: 130,
            g: 120,
            b: 105,
        },
        day_sky_b: Rgb {
            r: 155,
            g: 140,
            b: 125,
        },
        night_sky_a: Rgb {
            r: 20,
            g: 20,
            b: 20,
        },
        night_sky_b: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        twilight_a: Rgb {
            r: 254,
            g: 128,
            b: 25,
        },
        twilight_b: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        sun_spill: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        floor_lamp_halo: Rgb {
            r: 254,
            g: 128,
            b: 25,
        },
        desk_lamp: Rgb {
            r: 254,
            g: 190,
            b: 100,
        },
        night_tint: Rgb {
            r: 18,
            g: 18,
            b: 16,
        },
        // Moon brightened toward blue: gruvbox's own aqua leans green and would
        // not read as cool.
        sun_core: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        moon_core: Rgb {
            r: 110,
            g: 160,
            b: 170,
        },
    },
    furniture: FurnitureColors {
        wood_top: Rgb {
            r: 80,
            g: 73,
            b: 69,
        },
        wood_trim: Rgb {
            r: 60,
            g: 56,
            b: 54,
        },
        rug_field: Rgb {
            r: 70,
            g: 45,
            b: 35,
        },
        rug_trim: Rgb {
            r: 50,
            g: 30,
            b: 22,
        },
        rug_accent: Rgb {
            r: 254,
            g: 128,
            b: 25,
        },
        magazine: Rgb {
            r: 131,
            g: 165,
            b: 152,
        },
        magazine_trim: Rgb {
            r: 66,
            g: 82,
            b: 76,
        },
        chair_trim: Rgb {
            r: 45,
            g: 42,
            b: 38,
        },
        coffee_cup: Rgb {
            r: 168,
            g: 153,
            b: 132,
        },
        coffee_cup_shadow: Rgb {
            r: 146,
            g: 131,
            b: 116,
        },
        tank_water: Rgb {
            r: 0x2e,
            g: 0x5a,
            b: 0x66,
        },
        tank_water_line: Rgb {
            r: 0x83,
            g: 0xa5,
            b: 0x98,
        },
        tank_fish: Rgb {
            r: 0xfe,
            g: 0x80,
            b: 0x19,
        },
        tank_fish_alt: Rgb {
            r: 0xcc,
            g: 0x24,
            b: 0x1d,
        },
        tank_plant: Rgb {
            r: 0x98,
            g: 0x97,
            b: 0x1a,
        },
        paper: Rgb {
            r: 0xeb,
            g: 0xdb,
            b: 0xb2,
        },
        paper_shade: Rgb {
            r: 0xbd,
            g: 0xae,
            b: 0x93,
        },
    },
    effects: EffectColors {
        monitor_frame_lit: Rgb {
            r: 80,
            g: 73,
            b: 69,
        },
        monitor_idle: Rgb {
            r: 56,
            g: 84,
            b: 96,
        },
        sleep_z: Rgb {
            r: 131,
            g: 165,
            b: 152,
        },
        coffee_steam: Rgb {
            r: 168,
            g: 153,
            b: 132,
        },
        walking_dust: Rgb {
            r: 80,
            g: 73,
            b: 69,
        },
        requires_action_bubble: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
    },
    tool_glow: ToolGlowColors {
        edit: Rgb {
            r: 131,
            g: 165,
            b: 152,
        },
        read: Rgb {
            r: 142,
            g: 192,
            b: 124,
        },
        bash: Rgb {
            r: 254,
            g: 128,
            b: 25,
        },
        agent: Rgb {
            r: 211,
            g: 134,
            b: 155,
        },
        grep: Rgb {
            r: 184,
            g: 187,
            b: 38,
        },
        // Yellow, not the gruvbox blue: that one is byte-identical to `edit`, and
        // the remaining spare accents are all green-family.
        default: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
    },
    ui: UiColors {
        label_running: Rgb {
            r: 184,
            g: 187,
            b: 38,
        },
        label_ok: Rgb {
            r: 184,
            g: 187,
            b: 38,
        },
        label_requires_action: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        label_warn: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        label_idle: Rgb {
            r: 168,
            g: 153,
            b: 132,
        },
        label_exiting: Rgb {
            r: 150,
            g: 135,
            b: 120,
        },
        tooltip_bg: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        tooltip_title: Rgb {
            r: 235,
            g: 219,
            b: 178,
        },
        tooltip_text: Rgb {
            r: 189,
            g: 174,
            b: 147,
        },
        tooltip_dim: Rgb {
            r: 146,
            g: 131,
            b: 116,
        },
        neon_brand: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        neon_star: Rgb {
            r: 254,
            g: 128,
            b: 25,
        },
        neon_alert: Rgb {
            r: 251,
            g: 73,
            b: 52,
        },
    },
    appliance: ApplianceColors {
        vending_body: Rgb {
            r: 45,
            g: 42,
            b: 38,
        },
        vending_panel: Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        vending_drinks: [
            Rgb {
                r: 254,
                g: 128,
                b: 25,
            },
            Rgb {
                r: 142,
                g: 192,
                b: 124,
            },
            Rgb {
                r: 184,
                g: 187,
                b: 38,
            },
            Rgb {
                r: 211,
                g: 134,
                b: 155,
            },
        ],
        vending_trim: Rgb {
            r: 180,
            g: 135,
            b: 85,
        },
        vending_dark: Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        printer_body: Rgb {
            r: 220,
            g: 210,
            b: 195,
        },
        printer_top: Rgb {
            r: 70,
            g: 65,
            b: 60,
        },
        printer_glass: Rgb {
            r: 131,
            g: 165,
            b: 152,
        },
        printer_paper: Rgb {
            r: 240,
            g: 235,
            b: 225,
        },
        printer_tray: Rgb {
            r: 145,
            g: 135,
            b: 125,
        },
        coats: [
            Rgb {
                r: 240,
                g: 140,
                b: 80,
            },
            Rgb {
                r: 131,
                g: 165,
                b: 152,
            },
            Rgb {
                r: 235,
                g: 200,
                b: 140,
            },
        ],
    },
    source: SourceColors {
        claude_code: Rgb {
            r: 0xfe,
            g: 0x80,
            b: 0x19,
        }, // gruvbox bright orange
        codex: Rgb {
            r: 0x83,
            g: 0xa5,
            b: 0x98,
        }, // gruvbox aqua
        reasonix: Rgb {
            r: 0xd3,
            g: 0x86,
            b: 0x9b,
        }, // gruvbox pink
        antigravity: Rgb {
            r: 0xb8,
            g: 0xbb,
            b: 0x26,
        }, // gruvbox bright yellow-green
        codewhale: Rgb {
            r: 0x45,
            g: 0xa0,
            b: 0xa8,
        }, // gruvbox teal
        opencode: Rgb {
            r: 0xfb,
            g: 0x49,
            b: 0x34,
        }, // gruvbox red
        copilot: Rgb {
            r: 0xff,
            g: 0x7a,
            b: 0xb0,
        }, // copilot rose
        cursor: Rgb {
            r: 0xa0,
            g: 0xaa,
            b: 0xc3,
        }, // cursor slate-blue (monochrome brand)
        openclaw: Rgb {
            r: 0xff,
            g: 0xaa,
            b: 0x30,
        }, // openclaw marigold (must clear claude-amber + opencode-red)
        hermes: Rgb {
            r: 0x68,
            g: 0x58,
            b: 0xe0,
        }, // Hermes/Nous indigo (brand-constant; must clear reasonix-violet + cursor-slate + codex-blue)
        omp: Rgb {
            r: 0x8a,
            g: 0xc8,
            b: 0x32,
        }, // omp lime (brand-constant; must clear antigravity-green + codewhale-teal + claude-amber)
        grok: Rgb {
            r: 0x30,
            g: 0xd0,
            b: 0xe8,
        }, // grok cyan (brand-constant; nearest neighbor codewhale-teal, and the site's chip transforms must clear WCAG AA)
        kimi: Rgb {
            r: 0xc4,
            g: 0x46,
            b: 0xfe,
        }, // kimi electric violet (brand-constant; must clear every source on every theme)
        jcode: Rgb {
            r: 0x60,
            g: 0xc0,
            b: 0x80,
        },
        dsh: Rgb {
            r: 0x4d,
            g: 0x6b,
            b: 0xfe,
        }, // deepseek blue (brand-constant)
    },
};
