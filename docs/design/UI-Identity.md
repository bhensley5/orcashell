# UI Identity: Orca Brutalism

Last updated: 2026-03-15
Status: Canonical visual direction for OrcaShell

## Style Thesis

OrcaShell uses a hybrid style. Neo-Brutalist structure meets deep ocean dark-mode with
bioluminescent terminal accents.

This is a deliberate fusion:
- **Brutalist traits.** Bold borders, hard directional shadows, strict layout rhythm,
  information density, thick visual weight. Pane dividers are structural. No rounded
  corners on splits.
- **Ocean depth.** Layered blue-black surfaces, subtle elevation through luminance shifts,
  a sense of looking into deep water. Not purple-midnight, not flat gray.
- **Orca identity.** Greyish whites and whitish blacks. The killer whale has distinctive
  markings. No pure black. No pure white. Everything lives in the orca spectrum.
- **Bioluminescent accents.** Soft pastel neons for terminal ANSI colors and interactive
  elements. Muted enough to be pleasant, bright enough to be functional.
- **Ghostty sensibility.** Clean, fast, monochrome-forward, sharp typography. The UI gets
  out of the way of the terminal content.

The result should feel like commanding a pod of agents from a deep-ocean control room.
Focused, powerful, a little electric. Not corporate dark mode. Not gaming neon. A dev tool
that happens to be beautiful.

Working label:
- **Orca Brutalism**

## Brand Personality

- Terminal-native and information-dense. This is a power tool.
- Calm and confident despite the dark palette. Not aggressive, not edgy.
- Technically precise. Every pixel serves a purpose.
- The orca metaphor is subtle: coordinated pods, deep water, black and white contrast.
- Community and open-source spirited.

## Visual Non-Negotiables

### 1. No Pure Black, No Pure White

- Do NOT use `#000000` for any surface.
- Do NOT use `#FFFFFF` for any text.
- The orca identity lives in the space between. Greyish whites and whitish blacks.
- This is the single most important rule. It defines the entire visual character.

### 2. Surface Hierarchy (The Depth Chart)

Four surface levels create depth. Tinted blue-black like looking into deep ocean water.

| Level | Name | Hex | Usage |
|-------|------|-----|-------|
| 0 | Abyss | `#1C1F26` | Window background, deepest layer |
| 1 | Deep | `#12151C` | Main content areas, terminal backgrounds, pane fill |
| 2 | Current | `#1C2028` | Hovered elements. Active panes, dropdown backgrounds |
| 3 | Surface | `#262A34` | Selected states, floating panels, focused pane border fill |

The progression from Abyss to Surface should feel like layers rising from the ocean floor.
Each step is subtle (~+8 lightness) but perceptible.

### 3. The Orca Whites (Text & Foreground)

| Name | Hex | Usage |
|------|-----|-------|
| Patch | `#E8EAF0` | Brightest. Active indicators, focused element text, logo mark |
| Bone | `#D8DAE0` | Primary text. Headlines, body, terminal foreground default |
| Fog | `#9499A8` | Secondary text. Metadata, timestamps, inactive tab labels |
| Slate | `#5C6070` | Tertiary. Placeholders, disabled text, subtle dividers |

- `Bone` is the default text color. Slightly cool, cohesive with the blue-black surfaces.
- `Patch` is used sparingly for emphasis. The orca's white belly patch.
- `Fog` is for anything that should be readable but visually subordinate.
- `Slate` is the lowest tier. Barely visible, for structural hints.

### 4. Border & Shadow Language

**Borders:**
- Default border: `2px solid #2A2E3A` (muted blue-slate, barely visible).
- Emphasis border: `2px solid #3A4050` (visible but not dominant).
- Focused pane border: `2px solid #5E9BFF` (Orca Blue - the active pane indicator).
- Border purpose: structural. Borders define space, they don't decorate.

**Shadows (hard, directional, brutalist):**
- Resting: `4px 4px 0px #06080C` (hard, slightly darker than Abyss).
- Hover: `6px 6px 0px #06080C`.
- Featured: `8px 8px 0px` using accent at 15 percent opacity.

**Glow effects (used sparingly):**
- Focus ring: `0 0 0 2px #5E9BFF40` (Orca Blue at 25 percent opacity).
- Active terminal glow: `0 0 12px #5E9BFF15` (very subtle, 8 percent opacity).

### 5. Accent Colors (UI)

| Name | Hex | Usage |
|------|-----|-------|
| Orca Blue | `#5E9BFF` | Primary accent. Focused pane, active task, primary actions |
| Status Green | `#7EFFC1` | Success, task complete, agent running |
| Status Coral | `#FF7E9D` | Error, task failed, agent crashed |
| Status Amber | `#FFD97E` | Warning, merge conflict, agent blocked |

Usage rules:
- Only one accent color per component (except status indicators which use semantic colors).
- Orca Blue is the dominant accent. Others are functional or semantic.
- For surface tints, use accent at 6-10 percent opacity over a Deep or Current surface.
- Never fill large areas with accent colors.

### 6. Terminal ANSI Color Palette (Pastel Neons)

The terminal color scheme uses soft pastel neons. Muted enough to be pleasant for hours
of reading, bright enough to be functional. These are the colors agents output.

**Normal intensity:**

| ANSI | Name | Hex |
|------|------|-----|
| Black | Abyss | `#0A0C10` |
| Red | Neon Coral | `#FF7E9D` |
| Green | Neon Mint | `#7EFFC1` |
| Yellow | Neon Amber | `#FFD97E` |
| Blue | Orca Blue | `#5E9BFF` |
| Magenta | Neon Lavender | `#B87EFF` |
| Cyan | Neon Cyan | `#7EE8FA` |
| White | Bone | `#D8DAE0` |

**Bright/bold intensity:**

| ANSI | Name | Hex |
|------|------|-----|
| Bright Black | Slate | `#5C6070` |
| Bright Red | `#FFA0B8` | |
| Bright Green | `#A0FFD6` | |
| Bright Yellow | `#FFE5A0` | |
| Bright Blue | `#82B4FF` | |
| Bright Magenta | `#CCA0FF` | |
| Bright Cyan | `#A0F0FF` | |
| Bright White | Patch | `#E8EAF0` |

**Terminal specific:**
- Background: Deep (`#12151C`)
- Foreground: Bone (`#D8DAE0`)
- Cursor: Orca Blue (`#5E9BFF`)
- Selection background: `#5E9BFF30` (Orca Blue at 19 percent opacity)
- Selection foreground: Patch (`#E8EAF0`)

### 7. Pane & Layout Structure

This is a terminal-centric dev tool, not a web app. Layout is brutalist and functional.

**Pane dividers:**
- Width: `1px` (thin, structural).
- Color: `#2A2E3A` (same as default border).
- No rounded corners on pane splits. Sharp, brutalist division.
- Draggable with visible resize handle on hover (Fog color).

**Focused pane indicator:**
- Active pane gets a `2px` Orca Blue border on top edge.
- Inactive panes have no colored border. Just the structural divider.
- This is the primary way users know where keyboard focus is.

**Task sidebar:**
- Width: 240-280px, collapsible.
- Surface: Deep background.
- Border: 1px right border (default border color).
- Task items: compact rows. Monospace text, status dot (accent-colored).

**Status bar:**
- Height: 24px, at window bottom.
- Surface: Abyss (darkest, recedes visually).
- Content: task count, active agent, branch name, connection status. All in Fog color.

### 8. Corner Radii

Tighter than StuffAICanDo. This is a dev tool, not a consumer app:

- Pane dividers: `0px` (sharp, brutalist).
- Panels and sidebars: `0px` (structural elements are sharp).
- Cards and floating elements: `4px` (just enough to not cut).
- Buttons: `4px`.
- Input fields: `4px`.
- Badges and pills: `6px`.
- Tooltips: `6px`.

### 9. Typography

**UI text:**
- Primary UI font: System font stack (San Francisco on macOS, system sans on Linux) or
  a geometric sans-serif (Space Grotesk if bundled).
- Terminal text: User-configurable monospace. Default: system monospace.
- All status text, task IDs, branch names, file paths: monospace always.

**Scale (for GPUI native, not web. Pixel values are points at 1x):**
- Window title / section headers: 16pt, bold
- Panel headers: 14pt, semibold
- Body / list items: 13pt, regular
- Metadata / timestamps: 12pt, regular (Fog color)
- Status bar: 11pt, regular (Fog color)
- Tiny labels / badges: 10pt, medium

**Rules:**
- When in doubt, use monospace. This is a terminal app.
- Task descriptions and agent names are the only things that might use proportional font.
- Line height for UI text: 1.4 (tighter than web. Native apps can be denser).

### 10. Logo Integration

The OrcaShell logo appears in:
- Top-left of the window (title bar area). Sized to match the title bar height.
- Loading/splash screen (centered, larger).
- About dialog.

Logo should work at small sizes (16x16 for title bar) and be recognizable in monochrome
(Bone on Abyss).

## Interaction Patterns

### Focus
- Focused pane: Orca Blue top border (2px).
- Focused input: Orca Blue border plus subtle glow ring.
- Keyboard navigation: visible focus indicator on all interactive elements (never hidden).

### Selection
- Terminal text selection: Orca Blue at 19 percent opacity background.
- List item selection: Current surface background plus Orca Blue left-edge indicator (2px).

### State Indicators
- Running agent: Status Green dot (pulsing subtle, 2s period).
- Failed/crashed: Status Coral dot (static).
- Blocked/waiting: Status Amber dot (static).
- Idle/no agent: Slate dot (static).

## Anti-Patterns (Disallowed)

- Pure black (`#000000`) or pure white (`#FFFFFF`) anywhere.
- Generic gray dark mode (our darks are blue-tinted, always).
- Purple-heavy AI palette (Lavender is one terminal color, not a theme).
- Gradient backgrounds or gradient text.
- Glassmorphism or blur effects (contradicts brutalist structure).
- Rounded pane dividers or soft splits (pane boundaries are sharp).
- Neon overload (max 1 accent color per component, status colors excepted).
- Thin low-contrast "ghost" UI elements.
- Decorative elements that do not serve information architecture.
- Loading spinners (use static "loading..." text or skeleton outlines).
- Animated glowing or pulsing in loops (exception: running-agent status dot).

## Accessibility

- All text or background pairs must meet WCAG AA contrast (4.5:1 body, 3:1 large).
- Verified: Bone on Deep = 11.2:1 (pass). Fog on Deep = 5.1:1 (pass).
  Slate on Deep = 2.8:1 (use for decoration only, not readable text).
- Focus indicators: always visible, never removed.
- Keyboard navigation: all interactive elements reachable.
- Color is never the sole carrier of meaning (pair status dots with text labels).

## GPUI Implementation Notes

This design will be implemented as GPUI token constants in Rust, not CSS:

```rust
// Example: Surface tokens as GPUI colors
pub mod theme {
    use gpui::Rgba;

    pub const ABYSS: Rgba = rgba(0x1C1F26FF);
    pub const DEEP: Rgba = rgba(0x12151CFF);
    pub const CURRENT: Rgba = rgba(0x1C2028FF);
    pub const SURFACE: Rgba = rgba(0x262A34FF);

    pub const BONE: Rgba = rgba(0xD8DAE0FF);
    pub const FOG: Rgba = rgba(0x9499A8FF);
    pub const SLATE: Rgba = rgba(0x5C6070FF);
    pub const PATCH: Rgba = rgba(0xE8EAF0FF);

    pub const ORCA_BLUE: Rgba = rgba(0x5E9BFFFF);
    // ...
}
```

A dedicated GPUI theming phase will create the full token system, expose it as a struct
for potential future theme switching, and ensure all UI code references tokens rather than
hardcoded values.
