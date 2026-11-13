//! TesselGUI 1.4.0 design tokens — the single source of truth for the
//! colors of every SeanPak window (Builder studio, installer wizard,
//! uninstall confirmation). SeanPak ships dark only.
//!
//! Values are the TesselGUI dark theme from `docs/tokens.json` (the
//! GitHub Dark High Contrast values adopted in TesselGUI 1.3.0),
//! expressed as Win32 COLORREFs (`0x00BBGGRR`). Windows keep no
//! private palettes: they consume these constants and the shared
//! control painters below, so the three windows cannot drift apart
//! again the way the revision-2 copies did.

/// `background` — #010409: window background; also the wizard rail.
pub const BACKGROUND: u32 = 0x0009_0401;
/// `surface` — #0A0C10: panels, edit fields, list bodies.
pub const SURFACE: u32 = 0x0010_0C0A;
/// `raised` — #191C20: secondary button fill.
pub const RAISED: u32 = 0x0020_1C19;
/// `selected` — #272B33: selected row fill.
pub const SELECTED: u32 = 0x0033_2B27;
/// `selectedStrong` — #40454D.
pub const SELECTED_STRONG: u32 = 0x004D_4540;
/// `foreground` — #F0F3F6: enabled text.
pub const FOREGROUND: u32 = 0x00F6_F3F0;
/// `muted` — #BDC4CC: section captions and genuinely secondary text.
pub const MUTED: u32 = 0x00CC_C4BD;
/// `border` / `edge` — #7A828E: 1px structural edges and dividers.
pub const EDGE: u32 = 0x008E_827A;
/// `accent` — #15263C: deep accent fill (the wizard's info box).
pub const ACCENT: u32 = 0x003C_2615;
/// `accentForeground` — #F0F6FC: text on the accent fill.
pub const ACCENT_FOREGROUND: u32 = 0x00FC_F6F0;
/// `selectionIndicator` — #409EFF: the selected row's marker bar.
pub const SELECTION_INDICATOR: u32 = 0x00FF_9E40;
/// `focus` — #71B7FF: focus rings.
pub const FOCUS: u32 = 0x00FF_B771;
/// `action` — #1A7F37: the one primary action per view.
pub const ACTION: u32 = 0x0037_7F1A;
/// `actionHover` — #238636.
pub const ACTION_HOVER: u32 = 0x0036_8623;
/// `actionPressed` — #116329.
pub const ACTION_PRESSED: u32 = 0x0029_6311;
/// `actionForeground` — #FFFFFF: text on the action fill.
pub const ACTION_FOREGROUND: u32 = 0x00FF_FFFF;
/// `hover` fill — #20242B: hovered secondary button.
pub const HOVER_FILL: u32 = 0x002B_2420;
/// `pressed` fill — #2E333B: pressed secondary button.
pub const PRESSED_FILL: u32 = 0x003B_332E;
/// `success` — #3FB950: success text, completed-step checks.
pub const SUCCESS: u32 = 0x0050_B93F;
/// `warning` — #F0B72F.
pub const WARNING: u32 = 0x002F_B7F0;
/// `warningFill` — #3A2A00.
pub const WARNING_FILL: u32 = 0x0000_2A3A;
/// `danger` — #FFA198: error text.
pub const DANGER: u32 = 0x0098_A1FF;
/// `dangerFill` — #4C1A1A.
pub const DANGER_FILL: u32 = 0x001A_1A4C;
/// `info` — #A5D6FF: informational note text.
pub const INFO: u32 = 0x00FF_D6A5;
/// `infoFill` — #0C2D6B.
pub const INFO_FILL: u32 = 0x006B_2D0C;
/// `grid` — #0F1318: table gridlines.
pub const GRID: u32 = 0x0018_130F;
/// `disabledForeground` — #7A828E: text of disabled controls only.
pub const DISABLED_FOREGROUND: u32 = 0x008E_827A;
/// `disabledFill` — #0A0C10: fill of disabled controls.
pub const DISABLED_FILL: u32 = 0x0010_0C0A;
/// `readOnlyFill` — #11151B: read-only edit fields.
pub const READ_ONLY_FILL: u32 = 0x001B_1511;
/// `progress` — #BDC4CC (the progress role shares the muted value).
pub const PROGRESS: u32 = MUTED;

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a COLORREF (0x00BBGGRR) back to its #RRGGBB token text
    /// so the pins below read exactly like tokens.json.
    fn hex(color: u32) -> String {
        let r = color & 0xFF;
        let g = (color >> 8) & 0xFF;
        let b = (color >> 16) & 0xFF;
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    #[test]
    fn core_tokens_match_tesselgui_140() {
        assert_eq!(hex(BACKGROUND), "#010409");
        assert_eq!(hex(SURFACE), "#0A0C10");
        assert_eq!(hex(EDGE), "#7A828E");
        assert_eq!(hex(ACTION), "#1A7F37");
        assert_eq!(hex(FOCUS), "#71B7FF");
        assert_eq!(hex(DANGER), "#FFA198");
    }

    #[test]
    fn remaining_tokens_match_tesselgui_140() {
        assert_eq!(hex(RAISED), "#191C20");
        assert_eq!(hex(SELECTED), "#272B33");
        assert_eq!(hex(SELECTED_STRONG), "#40454D");
        assert_eq!(hex(FOREGROUND), "#F0F3F6");
        assert_eq!(hex(MUTED), "#BDC4CC");
        assert_eq!(hex(ACCENT), "#15263C");
        assert_eq!(hex(ACCENT_FOREGROUND), "#F0F6FC");
        assert_eq!(hex(SELECTION_INDICATOR), "#409EFF");
        assert_eq!(hex(ACTION_HOVER), "#238636");
        assert_eq!(hex(ACTION_PRESSED), "#116329");
        assert_eq!(hex(ACTION_FOREGROUND), "#FFFFFF");
        assert_eq!(hex(HOVER_FILL), "#20242B");
        assert_eq!(hex(PRESSED_FILL), "#2E333B");
        assert_eq!(hex(SUCCESS), "#3FB950");
        assert_eq!(hex(WARNING), "#F0B72F");
        assert_eq!(hex(WARNING_FILL), "#3A2A00");
        assert_eq!(hex(DANGER_FILL), "#4C1A1A");
        assert_eq!(hex(INFO), "#A5D6FF");
        assert_eq!(hex(INFO_FILL), "#0C2D6B");
        assert_eq!(hex(GRID), "#0F1318");
        assert_eq!(hex(DISABLED_FOREGROUND), "#7A828E");
        assert_eq!(hex(DISABLED_FILL), "#0A0C10");
        assert_eq!(hex(READ_ONLY_FILL), "#11151B");
        assert_eq!(hex(PROGRESS), "#BDC4CC");
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::Controls::*;
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::*;

    // Messages and values windows-sys keeps behind features this
    // crate does not enable; defined locally like the windows do.
    const WM_GETFONT_MSG: u32 = 0x0031;
    const BM_GETCHECK_MSG: u32 = 0x00F0;
    const BST_CHECKED_STATE: usize = 1;
    const LVM_GETHEADER_MSG: u32 = 0x101F;
    const WM_PRINTCLIENT_MSG: u32 = 0x0318;
    const NM_CUSTOMDRAW_CODE: i32 = -12;
    const CDDS_PREPAINT: u32 = 0x0000_0001;
    const CDDS_ITEM: u32 = 0x0001_0000;
    const CDDS_ITEMPREPAINT: u32 = CDDS_ITEM | 0x0001;
    const CDDS_ITEMPOSTPAINT: u32 = CDDS_ITEM | 0x0002;
    const CDDS_SUBITEM: u32 = 0x0002_0000;
    const CDRF_NEWFONT: isize = 0x0000_0002;
    const CDRF_NOTIFYPOSTPAINT: isize = 0x0000_0010;
    const CDRF_NOTIFYITEMDRAW: isize = 0x0000_0020;
    const CDRF_NOTIFYSUBITEMDRAW: isize = 0x0000_0020;
    const CDIS_SELECTED_FLAG: u32 = 0x0001;

    /// Process-wide solid-brush cache: one brush per token color,
    /// created on first use. Every window paints from this cache, so
    /// no window owns (or leaks, or frees early) a private palette.
    const CACHE_SLOTS: usize = 40;
    static mut BRUSH_CACHE: [(u32, HBRUSH); CACHE_SLOTS] =
        [(u32::MAX, std::ptr::null_mut()); CACHE_SLOTS];

    pub fn brush(color: u32) -> HBRUSH {
        unsafe {
            // Raw-pointer access: no references to the mutable
            // static are ever created (single UI-thread callers).
            let base = std::ptr::addr_of_mut!(BRUSH_CACHE) as *mut (u32, HBRUSH);
            for i in 0..CACHE_SLOTS {
                let slot = base.add(i);
                if (*slot).0 == color {
                    return (*slot).1;
                }
            }
            let created = CreateSolidBrush(color);
            for i in 0..CACHE_SLOTS {
                let slot = base.add(i);
                if (*slot).0 == u32::MAX {
                    *slot = (color, created);
                    break;
                }
            }
            created
        }
    }

    /// A control's current window text (owner-drawn controls still
    /// carry their label as window text; screen readers read it too).
    unsafe fn control_text(hwnd: HWND) -> Vec<u16> {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return vec![0];
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        buf.truncate(got.max(0) as usize);
        buf.push(0);
        buf
    }

    /// Selects the control's own font into the draw DC and returns
    /// the previously selected font for the caller to restore.
    unsafe fn select_control_font(hdc: HDC, hwnd: HWND) -> HGDIOBJ {
        let font = SendMessageW(hwnd, WM_GETFONT_MSG, 0, 0) as HFONT;
        if font.is_null() {
            return std::ptr::null_mut();
        }
        SelectObject(hdc, font as HGDIOBJ)
    }

    /// The TesselGUI focus ring for buttons and checkboxes: a 2px
    /// ring in the focus color, drawn just inside the control rect
    /// (the native-toolkit adaptation of the outline ring).
    unsafe fn draw_focus_ring(hdc: HDC, rc: &RECT) {
        let mut inner = *rc;
        inner.left += 1;
        inner.top += 1;
        inner.right -= 1;
        inner.bottom -= 1;
        let pen = CreatePen(PS_SOLID, 2, FOCUS);
        let old_pen = SelectObject(hdc, pen as HGDIOBJ);
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        Rectangle(hdc, inner.left, inner.top, inner.right, inner.bottom);
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
        DeleteObject(pen as HGDIOBJ);
    }

    /// Owner-drawn push button, TesselGUI component rules: exactly
    /// one primary (action family) per view; secondaries are raised
    /// fill + edge border + foreground text. Pressed, hover, disabled
    /// and keyboard-focus states all come from the draw item state.
    ///
    /// # Safety
    /// `dis` must be a valid DRAWITEMSTRUCT delivered with a
    /// WM_DRAWITEM message; its DC and control handle must be live
    /// for the duration of the call (the normal draw-item contract).
    pub unsafe fn draw_push_button(dis: &DRAWITEMSTRUCT, primary: bool) {
        let disabled = dis.itemState & ODS_DISABLED != 0;
        let pressed = dis.itemState & ODS_SELECTED != 0;
        let hot = dis.itemState & ODS_HOTLIGHT != 0;
        let focused = dis.itemState & ODS_FOCUS != 0;
        let (fill, text_color) = if disabled {
            (DISABLED_FILL, DISABLED_FOREGROUND)
        } else if primary {
            (
                if pressed {
                    ACTION_PRESSED
                } else if hot {
                    ACTION_HOVER
                } else {
                    ACTION
                },
                ACTION_FOREGROUND,
            )
        } else {
            (
                if pressed {
                    PRESSED_FILL
                } else if hot {
                    HOVER_FILL
                } else {
                    RAISED
                },
                FOREGROUND,
            )
        };
        FillRect(dis.hDC, &dis.rcItem, brush(fill));
        FrameRect(dis.hDC, &dis.rcItem, brush(EDGE));
        let old_font = select_control_font(dis.hDC, dis.hwndItem);
        SetBkMode(dis.hDC, TRANSPARENT as i32);
        SetTextColor(dis.hDC, text_color);
        let text = control_text(dis.hwndItem);
        let mut rc = dis.rcItem;
        DrawTextW(
            dis.hDC,
            text.as_ptr(),
            (text.len() - 1) as i32,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if !old_font.is_null() {
            SelectObject(dis.hDC, old_font);
        }
        if focused && !disabled {
            draw_focus_ring(dis.hDC, &dis.rcItem);
        }
    }

    /// Owner-drawn checkbox: the box glyph plus the label, both in
    /// palette colors (themed native checkboxes draw their captions
    /// with the system button-text color, ignoring the parent's
    /// WM_CTLCOLOR* colors — on a dark window that made enabled
    /// labels render near-black). Checked boxes fill with the action
    /// color and carry a light check; the label is foreground while
    /// enabled and the disabled foreground only when disabled.
    ///
    /// # Safety
    /// Same draw-item contract as [`draw_push_button`].
    pub unsafe fn draw_checkbox(dis: &DRAWITEMSTRUCT) {
        let disabled = dis.itemState & ODS_DISABLED != 0;
        let focused = dis.itemState & ODS_FOCUS != 0;
        let checked =
            SendMessageW(dis.hwndItem, BM_GETCHECK_MSG, 0, 0) as usize == BST_CHECKED_STATE;
        // The control sits on the window background everywhere it is
        // used; repaint that first so state changes leave no ghosts.
        FillRect(dis.hDC, &dis.rcItem, brush(BACKGROUND));
        let height = dis.rcItem.bottom - dis.rcItem.top;
        let side = (height - 8).clamp(12, 18);
        let box_rc = RECT {
            left: dis.rcItem.left + 1,
            top: dis.rcItem.top + (height - side) / 2,
            right: dis.rcItem.left + 1 + side,
            bottom: dis.rcItem.top + (height - side) / 2 + side,
        };
        let (box_fill, check_color) = if disabled {
            (DISABLED_FILL, DISABLED_FOREGROUND)
        } else if checked {
            (ACTION, ACTION_FOREGROUND)
        } else {
            (SURFACE, FOREGROUND)
        };
        FillRect(dis.hDC, &box_rc, brush(box_fill));
        FrameRect(dis.hDC, &box_rc, brush(EDGE));
        if checked {
            // A three-point check, thick enough to read at 12px.
            let pen = CreatePen(PS_SOLID, (side / 6).max(2), check_color);
            let old_pen = SelectObject(dis.hDC, pen as HGDIOBJ);
            let x = box_rc.left as f32;
            let y = box_rc.top as f32;
            let s = side as f32;
            let mut pt: POINT = std::mem::zeroed();
            MoveToEx(
                dis.hDC,
                (x + s * 0.20) as i32,
                (y + s * 0.52) as i32,
                &mut pt,
            );
            LineTo(dis.hDC, (x + s * 0.42) as i32, (y + s * 0.74) as i32);
            LineTo(dis.hDC, (x + s * 0.80) as i32, (y + s * 0.26) as i32);
            SelectObject(dis.hDC, old_pen);
            DeleteObject(pen as HGDIOBJ);
        }
        let old_font = select_control_font(dis.hDC, dis.hwndItem);
        SetBkMode(dis.hDC, TRANSPARENT as i32);
        SetTextColor(
            dis.hDC,
            if disabled {
                DISABLED_FOREGROUND
            } else {
                FOREGROUND
            },
        );
        let text = control_text(dis.hwndItem);
        let mut rc = dis.rcItem;
        rc.left = box_rc.right + 8;
        DrawTextW(
            dis.hDC,
            text.as_ptr(),
            (text.len() - 1) as i32,
            &mut rc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if !old_font.is_null() {
            SelectObject(dis.hDC, old_font);
        }
        if focused && !disabled {
            draw_focus_ring(dis.hDC, &dis.rcItem);
        }
    }

    /// Paints a list view's header control in theme colors by
    /// subclassing the header and painting it in its own WM_PAINT.
    ///
    /// Why not owner-draw (the 4.7.1 route, which never painted on a
    /// real desktop): `HDF_OWNERDRAW` WAS set on every item — both
    /// windows insert their columns before calling the helper — but
    /// `WM_DRAWITEM` for header items is delivered to the header's
    /// parent window, and the header's parent is the list view
    /// itself, which does not forward the message to the top-level
    /// window. The windows' `ODT_HEADER_CTL` draw-item arms were
    /// therefore dead code, and the header kept its native light
    /// rendering in SEAN's 4.7.1 field captures. A subclass proc on
    /// the header receives the header's own messages directly, so
    /// painting there cannot be routed away. Call once per list view,
    /// after the control exists (columns may come before or after).
    ///
    /// # Safety
    /// `list` must be a live SysListView32 window handle.
    pub unsafe fn subclass_list_header(list: HWND) {
        let header = SendMessageW(list, LVM_GETHEADER_MSG, 0, 0) as HWND;
        if header.is_null() {
            return;
        }
        SetWindowSubclass(header, Some(header_subclass_proc), HEADER_SUBCLASS_ID, 0);
    }

    /// Subclass id + the one header item currently pressed (mouse
    /// down on it), process-wide: only one header can be pressed at
    /// a time, and only the UI thread touches this.
    const HEADER_SUBCLASS_ID: usize = 0x5E41;
    static mut HEADER_PRESSED: (usize, i32) = (0, -1);

    unsafe fn header_pressed_item(hwnd: HWND) -> i32 {
        let slot = std::ptr::addr_of!(HEADER_PRESSED);
        if (*slot).0 == hwnd as usize {
            (*slot).1
        } else {
            -1
        }
    }

    unsafe fn set_header_pressed(hwnd: HWND, item: i32) {
        let slot = std::ptr::addr_of_mut!(HEADER_PRESSED);
        *slot = (hwnd as usize, item);
    }

    /// The header subclass proc: paints in WM_PAINT (see
    /// [`subclass_list_header`] for why this route), swallows
    /// background erasure so the native fill never flashes through,
    /// tracks the pressed item for its fill, and otherwise defers to
    /// the header's own proc — clicks, notifications, and keyboard
    /// behavior are unchanged.
    unsafe extern "system" fn header_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _uid_subclass: usize,
        _ref_data: usize,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                if !hdc.is_null() {
                    paint_header_dc(hwnd, hdc);
                    EndPaint(hwnd, &ps);
                }
                0
            }
            WM_PRINTCLIENT_MSG => {
                paint_header_dc(hwnd, wparam as HDC);
                0
            }
            WM_ERASEBKGND => 1,
            WM_LBUTTONDOWN => {
                let mut hit: HDHITTESTINFO = std::mem::zeroed();
                hit.iItem = -1;
                hit.pt = POINT {
                    x: (lparam & 0xFFFF) as i16 as i32,
                    y: ((lparam >> 16) & 0xFFFF) as i16 as i32,
                };
                SendMessageW(hwnd, HDM_HITTEST, 0, &mut hit as *mut _ as LPARAM);
                set_header_pressed(hwnd, hit.iItem);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            WM_LBUTTONUP | WM_CANCELMODE => {
                if header_pressed_item(hwnd) >= 0 {
                    set_header_pressed(hwnd, -1);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            WM_NCDESTROY => {
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                RemoveWindowSubclass(hwnd, Some(header_subclass_proc), HEADER_SUBCLASS_ID);
                result
            }
            _ => DefSubclassProc(hwnd, msg, wparam, lparam),
        }
    }

    /// The themed header paint itself: the whole client area takes
    /// the surface fill (so the space past the last column matches),
    /// each item draws its column text — read from the header item
    /// itself, so it can never drift from the column definition — in
    /// muted, a pressed item takes the pressed fill, and edge-token
    /// dividers run down each item's right side and along the
    /// header's bottom.
    unsafe fn paint_header_dc(hwnd: HWND, hdc: HDC) {
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        FillRect(hdc, &client, brush(SURFACE));
        let old_font = select_control_font(hdc, hwnd);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, MUTED);
        let pen = CreatePen(PS_SOLID, 1, EDGE);
        let old_pen = SelectObject(hdc, pen as HGDIOBJ);
        let mut pt: POINT = std::mem::zeroed();
        let count = SendMessageW(hwnd, HDM_GETITEMCOUNT, 0, 0) as i32;
        for i in 0..count {
            let mut rc: RECT = std::mem::zeroed();
            if SendMessageW(
                hwnd,
                HDM_GETITEMRECT,
                i as WPARAM,
                &mut rc as *mut _ as LPARAM,
            ) == 0
            {
                continue;
            }
            if header_pressed_item(hwnd) == i {
                FillRect(hdc, &rc, brush(PRESSED_FILL));
            }
            let mut text: Vec<u16> = vec![0; 128];
            let mut item: HDITEMW = std::mem::zeroed();
            item.mask = HDI_TEXT;
            item.pszText = text.as_mut_ptr();
            item.cchTextMax = text.len() as i32;
            if SendMessageW(
                hwnd,
                HDM_GETITEMW,
                i as WPARAM,
                &mut item as *mut _ as LPARAM,
            ) != 0
            {
                let len = text.iter().position(|c| *c == 0).unwrap_or(0);
                let mut text_rc = rc;
                text_rc.left += 6;
                text_rc.right -= 2;
                DrawTextW(
                    hdc,
                    text.as_ptr(),
                    len as i32,
                    &mut text_rc,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
            }
            MoveToEx(hdc, rc.right - 1, rc.top, &mut pt);
            LineTo(hdc, rc.right - 1, rc.bottom);
        }
        MoveToEx(hdc, client.left, client.bottom - 1, &mut pt);
        LineTo(hdc, client.right, client.bottom - 1);
        SelectObject(hdc, old_pen);
        DeleteObject(pen as HGDIOBJ);
        if !old_font.is_null() {
            SelectObject(hdc, old_font);
        }
    }

    /// NM_CUSTOMDRAW handler for the dark list views: selected rows
    /// take the `selected` fill (the native selection is a system
    /// highlight blue), and gridlines are drawn per subitem in the
    /// `grid` color at postpaint (the native LVS_EX_GRIDLINES color
    /// is a fixed light system color that cannot be themed). The
    /// selected row also gets the selectionIndicator bar at its left
    /// edge. Returns the LRESULT the parent's WM_NOTIFY must return,
    /// or None when `lparam` is not a list-view custom-draw.
    ///
    /// # Safety
    /// `lparam` must be the LPARAM of a WM_NOTIFY message (a valid
    /// NMHDR pointer); non-custom-draw notifications are detected
    /// from the header and left alone.
    pub unsafe fn listview_custom_draw(lparam: LPARAM, draw_grid: bool) -> Option<LRESULT> {
        let hdr = lparam as *const NMHDR;
        if hdr.is_null() || (*hdr).code as i32 != NM_CUSTOMDRAW_CODE {
            return None;
        }
        let cd = lparam as *mut NMLVCUSTOMDRAW;
        let stage = (*cd).nmcd.dwDrawStage;
        if stage == CDDS_PREPAINT {
            let mut flags = CDRF_NOTIFYITEMDRAW;
            if draw_grid {
                flags |= CDRF_NOTIFYPOSTPAINT | CDRF_NOTIFYSUBITEMDRAW;
            }
            return Some(flags);
        }
        if stage == CDDS_ITEMPREPAINT {
            if (*cd).nmcd.uItemState & CDIS_SELECTED_FLAG != 0 {
                (*cd).clrText = FOREGROUND;
                (*cd).clrTextBk = SELECTED;
            }
            // CDRF_NEWFONT is what makes the color changes above
            // take; the notify bits add the subitem postpaint stage
            // the gridlines are drawn at.
            return Some(if draw_grid {
                CDRF_NEWFONT | CDRF_NOTIFYSUBITEMDRAW | CDRF_NOTIFYPOSTPAINT
            } else {
                CDRF_NEWFONT
            });
        }
        if draw_grid && stage == (CDDS_SUBITEM | CDDS_ITEMPOSTPAINT | CDDS_ITEM) {
            let rc = (*cd).nmcd.rc;
            let hdc = (*cd).nmcd.hdc;
            let pen = CreatePen(PS_SOLID, 1, GRID);
            let old_pen = SelectObject(hdc, pen as HGDIOBJ);
            let mut pt: POINT = std::mem::zeroed();
            MoveToEx(hdc, rc.left, rc.bottom - 1, &mut pt);
            LineTo(hdc, rc.right, rc.bottom - 1);
            MoveToEx(hdc, rc.right - 1, rc.top, &mut pt);
            LineTo(hdc, rc.right - 1, rc.bottom);
            SelectObject(hdc, old_pen);
            DeleteObject(pen as HGDIOBJ);
            if (*cd).iSubItem == 0 && (*cd).nmcd.uItemState & CDIS_SELECTED_FLAG != 0 {
                let bar = RECT {
                    left: rc.left,
                    top: rc.top,
                    right: rc.left + 3,
                    bottom: rc.bottom,
                };
                FillRect(hdc, &bar, brush(SELECTION_INDICATOR));
            }
            return Some(0);
        }
        Some(0)
    }
}

#[cfg(windows)]
pub use win::{brush, draw_checkbox, draw_push_button, listview_custom_draw, subclass_list_header};
