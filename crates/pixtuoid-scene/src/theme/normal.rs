use pixtuoid_core::sprite::Rgb;

use super::*;

/// The default `normal` theme — the only light-mode palette.
pub static NORMAL: Theme = Theme {
    name: "normal",
    kind: ThemeKind::Light,
    surface: SurfaceColors {
        wall: Rgb {
            r: 56,
            g: 56,
            b: 70,
        },
        wall_trim: Rgb {
            r: 80,
            g: 80,
            b: 100,
        },
        carpet_base: Rgb {
            r: 150,
            g: 110,
            b: 72,
        },
        carpet_light: Rgb {
            r: 178,
            g: 138,
            b: 96,
        },
        carpet_dark: Rgb {
            r: 118,
            g: 82,
            b: 50,
        },
        window_frame: Rgb {
            r: 24,
            g: 24,
            b: 32,
        },
        bg_fallback: Rgb {
            r: 28,
            g: 32,
            b: 40,
        },
    },
    office: OfficeColors {
        room_wall_trim_light: Rgb {
            r: 110,
            g: 112,
            b: 128,
        },
        room_wall_trim_dark: Rgb {
            r: 40,
            g: 42,
            b: 54,
        },
        runner_base: Rgb {
            r: 160,
            g: 120,
            b: 70,
        },
        runner_stripe: Rgb {
            r: 140,
            g: 100,
            b: 55,
        },
        runner_edge: Rgb {
            r: 90,
            g: 60,
            b: 35,
        },
        neon_panel_bg: Rgb {
            r: 12,
            g: 14,
            b: 22,
        },
        building_dark: Rgb {
            r: 20,
            g: 22,
            b: 32,
        },
        building_light: Rgb {
            r: 60,
            g: 65,
            b: 82,
        },
        city_lit_windows: [
            Rgb {
                r: 252,
                g: 215,
                b: 110,
            },
            Rgb {
                r: 180,
                g: 220,
                b: 255,
            },
            Rgb {
                r: 255,
                g: 180,
                b: 140,
            },
        ],
        city_dark_window: Rgb {
            r: 30,
            g: 32,
            b: 44,
        },
        clock_rim: Rgb {
            r: 200,
            g: 200,
            b: 210,
        },
        clock_face: Rgb {
            r: 240,
            g: 240,
            b: 240,
        },
        clock_hand: Rgb {
            r: 20,
            g: 20,
            b: 25,
        },
        shadow: Rgb {
            r: 30,
            g: 25,
            b: 18,
        },
    },
    lighting: LightingColors {
        day_sky_a: Rgb {
            r: 120,
            g: 160,
            b: 200,
        },
        day_sky_b: Rgb {
            r: 160,
            g: 190,
            b: 220,
        },
        night_sky_a: Rgb {
            r: 18,
            g: 26,
            b: 52,
        },
        night_sky_b: Rgb {
            r: 28,
            g: 36,
            b: 70,
        },
        twilight_a: Rgb {
            r: 220,
            g: 130,
            b: 80,
        },
        twilight_b: Rgb {
            r: 240,
            g: 170,
            b: 110,
        },
        sun_spill: Rgb {
            r: 255,
            g: 230,
            b: 160,
        },
        floor_lamp_halo: Rgb {
            r: 255,
            g: 210,
            b: 130,
        },
        desk_lamp: Rgb {
            r: 255,
            g: 210,
            b: 130,
        },
        night_tint: Rgb {
            r: 18,
            g: 22,
            b: 38,
        },
        sun_core: Rgb {
            r: 255,
            g: 240,
            b: 170,
        },
        moon_core: Rgb {
            r: 236,
            g: 242,
            b: 255,
        },
    },
    furniture: FurnitureColors {
        wood_top: Rgb {
            r: 132,
            g: 88,
            b: 52,
        },
        wood_trim: Rgb {
            r: 78,
            g: 52,
            b: 28,
        },
        rug_field: Rgb {
            r: 140,
            g: 60,
            b: 50,
        },
        rug_trim: Rgb {
            r: 90,
            g: 40,
            b: 35,
        },
        rug_accent: Rgb {
            r: 190,
            g: 130,
            b: 80,
        },
        magazine: Rgb {
            r: 98,
            g: 122,
            b: 178,
        },
        magazine_trim: Rgb {
            r: 50,
            g: 60,
            b: 92,
        },
        chair_trim: Rgb {
            r: 60,
            g: 40,
            b: 22,
        },
        coffee_cup: Rgb {
            r: 200,
            g: 190,
            b: 170,
        },
        coffee_cup_shadow: Rgb {
            r: 180,
            g: 160,
            b: 130,
        },
        tank_water: Rgb {
            r: 0x2e,
            g: 0x6f,
            b: 0x86,
        },
        tank_water_line: Rgb {
            r: 0x9a,
            g: 0xdb,
            b: 0xe8,
        },
        tank_fish: Rgb {
            r: 0xe0,
            g: 0x86,
            b: 0x2f,
        },
        tank_fish_alt: Rgb {
            // Distinct from the lobster's carapace reds so the two read apart on
            // screen; the mascot harness no longer keys on those RGBs.
            r: 0xd9,
            g: 0x4f,
            b: 0x1f,
        },
        tank_plant: Rgb {
            r: 0x3f,
            g: 0x8f,
            b: 0x3a,
        },
        paper: Rgb {
            r: 0xe6,
            g: 0xe2,
            b: 0xd4,
        },
        paper_shade: Rgb {
            r: 0xc2,
            g: 0xbd,
            b: 0xac,
        },
    },
    effects: EffectColors {
        monitor_frame_lit: Rgb {
            r: 180,
            g: 200,
            b: 200,
        },
        monitor_idle: Rgb {
            r: 72,
            g: 112,
            b: 145,
        },
        sleep_z: Rgb {
            r: 110,
            g: 110,
            b: 140,
        },
        coffee_steam: Rgb {
            r: 190,
            g: 190,
            b: 210,
        },
        walking_dust: Rgb {
            r: 150,
            g: 120,
            b: 85,
        },
        requires_action_bubble: Rgb {
            r: 255,
            g: 215,
            b: 70,
        },
    },
    tool_glow: ToolGlowColors {
        edit: Rgb {
            r: 100,
            g: 160,
            b: 255,
        },
        read: Rgb {
            r: 80,
            g: 220,
            b: 240,
        },
        bash: Rgb {
            r: 240,
            g: 170,
            b: 80,
        },
        agent: Rgb {
            r: 200,
            g: 140,
            b: 255,
        },
        grep: Rgb {
            r: 180,
            g: 220,
            b: 120,
        },
        default: Rgb {
            r: 140,
            g: 240,
            b: 170,
        },
    },
    ui: UiColors {
        label_running: Rgb {
            r: 60,
            g: 220,
            b: 60,
        },
        label_ok: Rgb {
            r: 60,
            g: 220,
            b: 60,
        },
        label_requires_action: Rgb {
            r: 220,
            g: 200,
            b: 50,
        },
        label_warn: Rgb {
            r: 220,
            g: 200,
            b: 50,
        },
        label_idle: Rgb {
            r: 160,
            g: 160,
            b: 160,
        },
        label_exiting: Rgb {
            r: 130,
            g: 130,
            b: 130,
        },
        tooltip_bg: Rgb {
            r: 20,
            g: 22,
            b: 30,
        },
        tooltip_title: Rgb {
            r: 240,
            g: 240,
            b: 240,
        },
        tooltip_text: Rgb {
            r: 200,
            g: 200,
            b: 210,
        },
        tooltip_dim: Rgb {
            r: 140,
            g: 140,
            b: 150,
        },
        neon_brand: Rgb {
            r: 80,
            g: 240,
            b: 255,
        },
        neon_star: Rgb {
            r: 255,
            g: 100,
            b: 200,
        },
        neon_alert: Rgb {
            r: 220,
            g: 200,
            b: 50,
        },
    },
    appliance: ApplianceColors {
        vending_body: Rgb {
            r: 50,
            g: 55,
            b: 65,
        },
        vending_panel: Rgb {
            r: 180,
            g: 60,
            b: 60,
        },
        vending_drinks: [
            Rgb {
                r: 220,
                g: 50,
                b: 50,
            },
            Rgb {
                r: 50,
                g: 160,
                b: 50,
            },
            Rgb {
                r: 50,
                g: 80,
                b: 200,
            },
            Rgb {
                r: 220,
                g: 180,
                b: 40,
            },
        ],
        vending_trim: Rgb {
            r: 180,
            g: 170,
            b: 100,
        },
        vending_dark: Rgb {
            r: 40,
            g: 42,
            b: 48,
        },
        printer_body: Rgb {
            r: 220,
            g: 220,
            b: 225,
        },
        printer_top: Rgb {
            r: 60,
            g: 60,
            b: 68,
        },
        printer_glass: Rgb {
            r: 130,
            g: 180,
            b: 200,
        },
        printer_paper: Rgb {
            r: 245,
            g: 245,
            b: 240,
        },
        printer_tray: Rgb {
            r: 180,
            g: 180,
            b: 185,
        },
        coats: [
            Rgb {
                r: 200,
                g: 60,
                b: 60,
            },
            Rgb {
                r: 80,
                g: 120,
                b: 200,
            },
            Rgb {
                r: 240,
                g: 240,
                b: 240,
            },
        ],
    },
    source: SourceColors {
        claude_code: Rgb {
            r: 0xc8,
            g: 0x6e,
            b: 0x12,
        }, // amber
        codex: Rgb {
            r: 0x1e,
            g: 0x80,
            b: 0xc0,
        }, // blue
        reasonix: Rgb {
            r: 0x9c,
            g: 0x3c,
            b: 0xc0,
        }, // violet
        antigravity: Rgb {
            r: 0x2e,
            g: 0x9e,
            b: 0x4a,
        }, // green
        codewhale: Rgb {
            r: 0x14,
            g: 0xb8,
            b: 0xb0,
        }, // teal
        opencode: Rgb {
            r: 0xd8,
            g: 0x3a,
            b: 0x3a,
        }, // red
        copilot: Rgb {
            r: 0xe0,
            g: 0x60,
            b: 0x9c,
        }, // copilot rose
        cursor: Rgb {
            r: 0x96,
            g: 0xa0,
            b: 0xaf,
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
        dsh: Rgb {
            r: 0x4d,
            g: 0x6b,
            b: 0xfe,
        }, // deepseek blue (brand-constant; the tightest neighbour — the source-separation floor and the site's badge-contrast sweep both gate it)
    },
};
