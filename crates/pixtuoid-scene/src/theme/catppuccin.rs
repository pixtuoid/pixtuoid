use pixtuoid_core::sprite::Rgb;

use super::*;

/// Catppuccin Mocha — warm pastels on dark chocolate.
/// Based on <https://github.com/catppuccin/catppuccin>
pub(crate) static CATPPUCCIN: Theme = Theme {
    name: "catppuccin",
    kind: ThemeKind::Dark,
    surface: SurfaceColors {
        wall: Rgb {
            r: 30,
            g: 30,
            b: 46,
        },
        wall_trim: Rgb {
            r: 69,
            g: 71,
            b: 90,
        },
        carpet_base: Rgb {
            r: 49,
            g: 50,
            b: 68,
        },
        carpet_light: Rgb {
            r: 59,
            g: 60,
            b: 80,
        },
        carpet_dark: Rgb {
            r: 39,
            g: 39,
            b: 55,
        },
        window_frame: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        bg_fallback: Rgb {
            r: 30,
            g: 30,
            b: 46,
        },
    },
    office: OfficeColors {
        room_wall_trim_light: Rgb {
            r: 69,
            g: 71,
            b: 90,
        },
        room_wall_trim_dark: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        runner_base: Rgb {
            r: 55,
            g: 50,
            b: 65,
        },
        runner_stripe: Rgb {
            r: 65,
            g: 58,
            b: 78,
        },
        runner_edge: Rgb {
            r: 39,
            g: 36,
            b: 50,
        },
        neon_panel_bg: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        building_dark: Rgb {
            r: 20,
            g: 20,
            b: 32,
        },
        building_light: Rgb {
            r: 70,
            g: 72,
            b: 95,
        },
        city_lit_windows: [
            Rgb {
                r: 249,
                g: 226,
                b: 175,
            },
            Rgb {
                r: 245,
                g: 194,
                b: 231,
            },
            Rgb {
                r: 148,
                g: 226,
                b: 213,
            },
        ],
        city_dark_window: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        clock_rim: Rgb {
            r: 180,
            g: 190,
            b: 254,
        },
        clock_face: Rgb {
            r: 205,
            g: 214,
            b: 244,
        },
        clock_hand: Rgb {
            r: 30,
            g: 30,
            b: 46,
        },
        shadow: Rgb {
            r: 17,
            g: 17,
            b: 27,
        },
    },
    lighting: LightingColors {
        day_sky_a: Rgb {
            r: 110,
            g: 115,
            b: 150,
        },
        day_sky_b: Rgb {
            r: 135,
            g: 140,
            b: 175,
        },
        night_sky_a: Rgb {
            r: 17,
            g: 17,
            b: 27,
        },
        night_sky_b: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        twilight_a: Rgb {
            r: 250,
            g: 179,
            b: 135,
        },
        twilight_b: Rgb {
            r: 245,
            g: 194,
            b: 231,
        },
        sun_spill: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
        floor_lamp_halo: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
        desk_lamp: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
        night_tint: Rgb {
            r: 17,
            g: 17,
            b: 27,
        },
        // Catppuccin peach sun / lavender moon.
        sun_core: Rgb {
            r: 250,
            g: 179,
            b: 135,
        },
        moon_core: Rgb {
            r: 180,
            g: 190,
            b: 254,
        },
    },
    furniture: FurnitureColors {
        wood_top: Rgb {
            r: 69,
            g: 71,
            b: 90,
        },
        wood_trim: Rgb {
            r: 49,
            g: 50,
            b: 68,
        },
        rug_field: Rgb {
            r: 60,
            g: 45,
            b: 65,
        },
        rug_trim: Rgb {
            r: 42,
            g: 32,
            b: 48,
        },
        rug_accent: Rgb {
            r: 203,
            g: 166,
            b: 247,
        },
        magazine: Rgb {
            r: 137,
            g: 180,
            b: 250,
        },
        magazine_trim: Rgb {
            r: 68,
            g: 90,
            b: 125,
        },
        chair_trim: Rgb {
            r: 39,
            g: 39,
            b: 55,
        },
        coffee_cup: Rgb {
            r: 127,
            g: 132,
            b: 156,
        },
        coffee_cup_shadow: Rgb {
            r: 108,
            g: 112,
            b: 134,
        },
        tank_water: Rgb {
            r: 0x45,
            g: 0x64,
            b: 0x7e,
        },
        tank_water_line: Rgb {
            r: 0x89,
            g: 0xdc,
            b: 0xeb,
        },
        tank_fish: Rgb {
            r: 0xfa,
            g: 0xb3,
            b: 0x87,
        },
        tank_fish_alt: Rgb {
            r: 0xf3,
            g: 0x8b,
            b: 0xa8,
        },
        tank_plant: Rgb {
            r: 0xa6,
            g: 0xe3,
            b: 0xa1,
        },
        paper: Rgb {
            r: 0xf2,
            g: 0xe9,
            b: 0xde,
        },
        paper_shade: Rgb {
            r: 0xc4,
            g: 0xba,
            b: 0xa8,
        },
    },
    effects: EffectColors {
        monitor_frame_lit: Rgb {
            r: 69,
            g: 71,
            b: 90,
        },
        monitor_idle: Rgb {
            r: 86,
            g: 104,
            b: 150,
        },
        sleep_z: Rgb {
            r: 137,
            g: 220,
            b: 235,
        },
        coffee_steam: Rgb {
            r: 203,
            g: 166,
            b: 247,
        },
        walking_dust: Rgb {
            r: 59,
            g: 60,
            b: 80,
        },
        requires_action_bubble: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
    },
    tool_glow: ToolGlowColors {
        edit: Rgb {
            r: 137,
            g: 180,
            b: 250,
        },
        read: Rgb {
            r: 116,
            g: 199,
            b: 236,
        },
        bash: Rgb {
            r: 250,
            g: 179,
            b: 135,
        },
        agent: Rgb {
            r: 203,
            g: 166,
            b: 247,
        },
        grep: Rgb {
            r: 166,
            g: 227,
            b: 161,
        },
        default: Rgb {
            r: 148,
            g: 226,
            b: 213,
        },
    },
    ui: UiColors {
        label_running: Rgb {
            r: 166,
            g: 227,
            b: 161,
        },
        label_ok: Rgb {
            r: 166,
            g: 227,
            b: 161,
        },
        label_requires_action: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
        label_warn: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
        label_idle: Rgb {
            r: 147,
            g: 153,
            b: 178,
        },
        label_exiting: Rgb {
            r: 127,
            g: 132,
            b: 156,
        },
        tooltip_bg: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        tooltip_title: Rgb {
            r: 205,
            g: 214,
            b: 244,
        },
        tooltip_text: Rgb {
            r: 166,
            g: 173,
            b: 200,
        },
        tooltip_dim: Rgb {
            r: 108,
            g: 112,
            b: 134,
        },
        neon_brand: Rgb {
            r: 137,
            g: 180,
            b: 250,
        },
        neon_star: Rgb {
            r: 245,
            g: 194,
            b: 231,
        },
        neon_alert: Rgb {
            r: 249,
            g: 226,
            b: 175,
        },
    },
    appliance: ApplianceColors {
        vending_body: Rgb {
            r: 55,
            g: 56,
            b: 74,
        },
        vending_panel: Rgb {
            r: 137,
            g: 180,
            b: 250,
        },
        vending_drinks: [
            Rgb {
                r: 250,
                g: 179,
                b: 135,
            },
            Rgb {
                r: 116,
                g: 199,
                b: 236,
            },
            Rgb {
                r: 166,
                g: 227,
                b: 161,
            },
            Rgb {
                r: 203,
                g: 166,
                b: 247,
            },
        ],
        vending_trim: Rgb {
            r: 210,
            g: 180,
            b: 160,
        },
        vending_dark: Rgb {
            r: 24,
            g: 24,
            b: 37,
        },
        printer_body: Rgb {
            r: 205,
            g: 214,
            b: 244,
        },
        printer_top: Rgb {
            r: 69,
            g: 71,
            b: 90,
        },
        printer_glass: Rgb {
            r: 148,
            g: 226,
            b: 213,
        },
        printer_paper: Rgb {
            r: 245,
            g: 240,
            b: 235,
        },
        printer_tray: Rgb {
            r: 137,
            g: 140,
            b: 165,
        },
        coats: [
            Rgb {
                r: 250,
                g: 179,
                b: 135,
            },
            Rgb {
                r: 137,
                g: 180,
                b: 250,
            },
            Rgb {
                r: 203,
                g: 166,
                b: 247,
            },
        ],
    },
    source: SourceColors {
        claude_code: Rgb {
            r: 0xfa,
            g: 0xb3,
            b: 0x87,
        }, // catppuccin peach
        codex: Rgb {
            r: 0x89,
            g: 0xdc,
            b: 0xeb,
        }, // catppuccin sky
        reasonix: Rgb {
            r: 0xcb,
            g: 0xa6,
            b: 0xf7,
        }, // catppuccin mauve
        antigravity: Rgb {
            r: 0xa6,
            g: 0xe3,
            b: 0xa1,
        }, // catppuccin green
        codewhale: Rgb {
            r: 0x33,
            g: 0xc0,
            b: 0xac,
        }, // deep teal — pulled off the pale sky codex uses (badge legibility)
        opencode: Rgb {
            r: 0xf3,
            g: 0x8b,
            b: 0xa8,
        }, // catppuccin red
        copilot: Rgb {
            r: 0xe0,
            g: 0x60,
            b: 0x9c,
        }, // copilot rose
        cursor: Rgb {
            r: 0x96,
            g: 0xa2,
            b: 0xbe,
        }, // cursor slate-blue (monochrome brand)
        openclaw: Rgb {
            r: 0xff,
            g: 0xaa,
            b: 0x30,
        }, // openclaw marigold (lobster; warm, clears claude-amber + opencode-red)
        hermes: Rgb {
            r: 0x68,
            g: 0x58,
            b: 0xe0,
        }, // Hermes/Nous indigo (brand-constant; clears reasonix-violet + cursor-slate + codex-blue on every theme)
        omp: Rgb {
            r: 0x8a,
            g: 0xc8,
            b: 0x32,
        }, // omp lime (brand-constant; clears antigravity-green + codewhale-teal + claude-amber on every theme)
        grok: Rgb {
            r: 0x30,
            g: 0xd0,
            b: 0xe8,
        }, // grok cyan (brand-constant; clears the Manhattan floor vs codewhale-teal, and the SITE's chip transforms clear WCAG AA in both themes — a lighter silver failed the day chip)
        kimi: Rgb {
            r: 0xc4,
            g: 0x46,
            b: 0xfe,
        }, // kimi electric violet (brand-constant; clears every other source on every theme)
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
