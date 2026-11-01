use pixtuoid_core::sprite::Rgb;

use super::*;

/// The `dracula` dark theme.
pub(crate) static DRACULA: Theme = Theme {
    name: "dracula",
    kind: ThemeKind::Dark,
    surface: SurfaceColors {
        wall: Rgb {
            r: 40,
            g: 42,
            b: 54,
        },
        wall_trim: Rgb {
            r: 68,
            g: 71,
            b: 90,
        },
        carpet_base: Rgb {
            r: 52,
            g: 45,
            b: 62,
        },
        carpet_light: Rgb {
            r: 65,
            g: 55,
            b: 78,
        },
        carpet_dark: Rgb {
            r: 38,
            g: 34,
            b: 48,
        },
        window_frame: Rgb {
            r: 30,
            g: 31,
            b: 40,
        },
        bg_fallback: Rgb {
            r: 40,
            g: 42,
            b: 54,
        },
    },
    office: OfficeColors {
        room_wall_trim_light: Rgb {
            r: 68,
            g: 71,
            b: 90,
        },
        room_wall_trim_dark: Rgb {
            r: 30,
            g: 31,
            b: 40,
        },
        runner_base: Rgb {
            r: 60,
            g: 45,
            b: 75,
        },
        runner_stripe: Rgb {
            r: 75,
            g: 55,
            b: 95,
        },
        runner_edge: Rgb {
            r: 40,
            g: 30,
            b: 52,
        },
        neon_panel_bg: Rgb {
            r: 30,
            g: 31,
            b: 40,
        },
        building_dark: Rgb {
            r: 25,
            g: 26,
            b: 35,
        },
        building_light: Rgb {
            r: 70,
            g: 72,
            b: 95,
        },
        city_lit_windows: [
            Rgb {
                r: 255,
                g: 184,
                b: 108,
            },
            Rgb {
                r: 80,
                g: 250,
                b: 123,
            },
            Rgb {
                r: 189,
                g: 147,
                b: 249,
            },
        ],
        city_dark_window: Rgb {
            r: 30,
            g: 31,
            b: 40,
        },
        clock_rim: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
        clock_face: Rgb {
            r: 248,
            g: 248,
            b: 242,
        },
        clock_hand: Rgb {
            r: 40,
            g: 42,
            b: 54,
        },
        shadow: Rgb {
            r: 20,
            g: 20,
            b: 28,
        },
    },
    lighting: LightingColors {
        day_sky_a: Rgb {
            r: 130,
            g: 110,
            b: 170,
        },
        day_sky_b: Rgb {
            r: 160,
            g: 135,
            b: 195,
        },
        night_sky_a: Rgb {
            r: 30,
            g: 20,
            b: 45,
        },
        night_sky_b: Rgb {
            r: 45,
            g: 30,
            b: 65,
        },
        twilight_a: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
        twilight_b: Rgb {
            r: 255,
            g: 121,
            b: 198,
        },
        sun_spill: Rgb {
            r: 255,
            g: 184,
            b: 108,
        },
        floor_lamp_halo: Rgb {
            r: 255,
            g: 121,
            b: 198,
        },
        desk_lamp: Rgb {
            r: 255,
            g: 202,
            b: 140,
        },
        night_tint: Rgb {
            r: 25,
            g: 15,
            b: 35,
        },
        sun_core: Rgb {
            r: 255,
            g: 184,
            b: 108,
        },
        moon_core: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
    },
    furniture: FurnitureColors {
        wood_top: Rgb {
            r: 72,
            g: 58,
            b: 85,
        },
        wood_trim: Rgb {
            r: 48,
            g: 38,
            b: 60,
        },
        rug_field: Rgb {
            r: 70,
            g: 35,
            b: 65,
        },
        rug_trim: Rgb {
            r: 48,
            g: 22,
            b: 45,
        },
        rug_accent: Rgb {
            r: 255,
            g: 121,
            b: 198,
        },
        magazine: Rgb {
            r: 139,
            g: 233,
            b: 253,
        },
        magazine_trim: Rgb {
            r: 70,
            g: 116,
            b: 126,
        },
        chair_trim: Rgb {
            r: 38,
            g: 39,
            b: 50,
        },
        coffee_cup: Rgb {
            r: 130,
            g: 128,
            b: 145,
        },
        coffee_cup_shadow: Rgb {
            r: 100,
            g: 98,
            b: 115,
        },
        tank_water: Rgb {
            r: 0x2b,
            g: 0x4a,
            b: 0x6f,
        },
        tank_water_line: Rgb {
            r: 0x8b,
            g: 0xe9,
            b: 0xfd,
        },
        tank_fish: Rgb {
            r: 0xff,
            g: 0xb8,
            b: 0x6c,
        },
        tank_fish_alt: Rgb {
            r: 0xff,
            g: 0x55,
            b: 0x55,
        },
        tank_plant: Rgb {
            r: 0x50,
            g: 0xfa,
            b: 0x7a,
        },
        paper: Rgb {
            r: 0xf0,
            g: 0xec,
            b: 0xf4,
        },
        paper_shade: Rgb {
            r: 0xc0,
            g: 0xba,
            b: 0xd0,
        },
    },
    effects: EffectColors {
        monitor_frame_lit: Rgb {
            r: 98,
            g: 114,
            b: 164,
        },
        monitor_idle: Rgb {
            r: 72,
            g: 88,
            b: 140,
        },
        sleep_z: Rgb {
            r: 139,
            g: 233,
            b: 253,
        },
        coffee_steam: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
        walking_dust: Rgb {
            r: 68,
            g: 64,
            b: 80,
        },
        requires_action_bubble: Rgb {
            r: 241,
            g: 250,
            b: 140,
        },
    },
    tool_glow: ToolGlowColors {
        edit: Rgb {
            r: 139,
            g: 233,
            b: 253,
        },
        read: Rgb {
            r: 255,
            g: 121,
            b: 198,
        },
        bash: Rgb {
            r: 255,
            g: 184,
            b: 108,
        },
        agent: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
        grep: Rgb {
            r: 80,
            g: 250,
            b: 123,
        },
        // The palette's one unclaimed accent: it must not collide with `edit`,
        // or `ToolKind::Other` and a turn between items glow like an edit.
        default: Rgb {
            r: 241,
            g: 250,
            b: 140,
        },
    },
    ui: UiColors {
        label_running: Rgb {
            r: 80,
            g: 250,
            b: 123,
        },
        label_ok: Rgb {
            r: 80,
            g: 250,
            b: 123,
        },
        label_requires_action: Rgb {
            r: 241,
            g: 250,
            b: 140,
        },
        label_warn: Rgb {
            r: 241,
            g: 250,
            b: 140,
        },
        label_idle: Rgb {
            r: 150,
            g: 162,
            b: 204,
        },
        label_exiting: Rgb {
            r: 126,
            g: 136,
            b: 172,
        },
        tooltip_bg: Rgb {
            r: 30,
            g: 31,
            b: 40,
        },
        tooltip_title: Rgb {
            r: 248,
            g: 248,
            b: 242,
        },
        tooltip_text: Rgb {
            r: 189,
            g: 187,
            b: 205,
        },
        tooltip_dim: Rgb {
            r: 98,
            g: 114,
            b: 164,
        },
        neon_brand: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
        neon_star: Rgb {
            r: 255,
            g: 121,
            b: 198,
        },
        neon_alert: Rgb {
            r: 241,
            g: 250,
            b: 140,
        },
    },
    appliance: ApplianceColors {
        vending_body: Rgb {
            r: 45,
            g: 47,
            b: 62,
        },
        vending_panel: Rgb {
            r: 189,
            g: 147,
            b: 249,
        },
        vending_drinks: [
            Rgb {
                r: 139,
                g: 233,
                b: 253,
            },
            Rgb {
                r: 255,
                g: 121,
                b: 198,
            },
            Rgb {
                r: 255,
                g: 184,
                b: 108,
            },
            Rgb {
                r: 80,
                g: 250,
                b: 123,
            },
        ],
        vending_trim: Rgb {
            r: 200,
            g: 150,
            b: 80,
        },
        vending_dark: Rgb {
            r: 25,
            g: 26,
            b: 35,
        },
        printer_body: Rgb {
            r: 230,
            g: 228,
            b: 240,
        },
        printer_top: Rgb {
            r: 65,
            g: 67,
            b: 85,
        },
        printer_glass: Rgb {
            r: 98,
            g: 114,
            b: 164,
        },
        printer_paper: Rgb {
            r: 240,
            g: 238,
            b: 248,
        },
        printer_tray: Rgb {
            r: 140,
            g: 138,
            b: 160,
        },
        coats: [
            Rgb {
                r: 255,
                g: 121,
                b: 198,
            },
            Rgb {
                r: 139,
                g: 233,
                b: 253,
            },
            Rgb {
                r: 248,
                g: 248,
                b: 242,
            },
        ],
    },
    source: SourceColors {
        claude_code: Rgb {
            r: 0xff,
            g: 0xb8,
            b: 0x6c,
        }, // dracula orange
        codex: Rgb {
            r: 0x8b,
            g: 0xe9,
            b: 0xfd,
        }, // dracula cyan
        reasonix: Rgb {
            r: 0xbd,
            g: 0x93,
            b: 0xf9,
        }, // dracula purple
        antigravity: Rgb {
            r: 0x50,
            g: 0xfa,
            b: 0x7b,
        }, // dracula green
        codewhale: Rgb {
            r: 0x33,
            g: 0xb8,
            b: 0xa8,
        }, // deep teal
        opencode: Rgb {
            r: 0xff,
            g: 0x55,
            b: 0x55,
        }, // dracula red
        copilot: Rgb {
            r: 0xe0,
            g: 0x60,
            b: 0x9c,
        }, // copilot rose
        cursor: Rgb {
            r: 0x96,
            g: 0xa0,
            b: 0xb9,
        }, // cursor slate-blue (monochrome brand; distinct from all 7)
        openclaw: Rgb {
            r: 0xff,
            g: 0xaa,
            b: 0x30,
        }, // openclaw marigold (lobster; warm, clears claude-amber + opencode-red)
        hermes: Rgb {
            r: 0x68,
            g: 0x58,
            b: 0xe0,
        }, // Hermes/Nous indigo (brand-constant)
        omp: Rgb {
            r: 0x8a,
            g: 0xc8,
            b: 0x32,
        }, // omp lime (brand-constant)
        grok: Rgb {
            r: 0x30,
            g: 0xd0,
            b: 0xe8,
        }, // grok cyan (brand-constant)
        kimi: Rgb {
            r: 0xc4,
            g: 0x46,
            b: 0xfe,
        }, // kimi electric violet (brand-constant)
        jcode: Rgb {
            r: 0x60,
            g: 0xc0,
            b: 0x80,
        },
        dsh: Rgb {
            r: 0x4d,
            g: 0x6b,
            b: 0xfe,
        }, // deepseek blue (brand-constant; the tightest neighbour — the source-separation floor and the site's badge-contrast sweep both gate it)
    },
};
