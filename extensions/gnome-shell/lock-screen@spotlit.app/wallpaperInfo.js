// SPDX-License-Identifier: Apache-2.0

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Layout from 'resource:///org/gnome/shell/ui/layout.js';

const MAX_INFO_BYTES = 16 * 1024;

const InformationLabel = GObject.registerClass(
class InformationLabel extends St.Label {
    _init(maxLines, params) {
        super._init(params);
        this._maxLines = maxLines;
        this.clutter_text.set({
            single_line_mode: false,
            line_wrap: true,
            line_wrap_mode: Pango.WrapMode.WORD_CHAR,
            ellipsize: Pango.EllipsizeMode.END,
        });
    }

    vfunc_get_preferred_height(forWidth) {
        // Reserve wrapped lines, as GNOME does for expanded notification text.
        const [, lineHeight] = super.vfunc_get_preferred_height(-1);
        const [, naturalHeight] = super.vfunc_get_preferred_height(forWidth);
        const height = Math.ceil(Math.min(naturalHeight, lineHeight * this._maxLines));
        return [height, height];
    }
});

export function parseWallpaperInfo(contents, imageName) {
    const info = JSON.parse(contents);
    if (info?.version !== 1 || info.image !== imageName ||
        typeof info.wallpaper_id !== 'string' || !info.wallpaper_id)
        return null;

    const clean = (value, limit) => typeof value === 'string'
        ? [...value.replace(/[\x00-\x1f\x7f-\x9f]/g, ' ').replace(/\s+/g, ' ').trim()]
            .slice(0, limit).join('')
        : '';
    const title = clean(info.title, 200);
    const description = clean(info.description, 600);
    const credit = clean(info.credit, 600);
    return title || description || credit ? {
        title,
        description: description !== title ? description : '',
        credit: credit !== title && credit !== description ? credit : '',
    } : null;
}

export class WallpaperInfo {
    constructor(dialog, settings) {
        this._dialog = dialog;
        this._settings = settings;
        this._backgroundSettings = new Gio.Settings({schema_id: 'org.gnome.desktop.screensaver'});
        this._themeContext = St.ThemeContext.get_for_stage(global.stage);
        this._signals = [];
        this._generation = 0;
        this._layoutId = 0;
        this._hasInfo = false;
        this._fits = false;
        this._destroyed = false;

        this._holder = new St.Widget({
            layout_manager: new Clutter.FixedLayout(),
            reactive: false,
            can_focus: false,
        });
        this._holder.add_constraint(new Layout.MonitorConstraint({primary: true}));
        this._panel = new St.BoxLayout({
            style_class: 'spotlit-wallpaper-info',
            orientation: Clutter.Orientation.VERTICAL,
            reactive: false,
            can_focus: false,
            visible: false,
            opacity: 0,
        });
        this._title = new InformationLabel(2, {style_class: 'spotlit-wallpaper-info-title'});
        this._description = new InformationLabel(4, {style_class: 'spotlit-wallpaper-info-details'});
        this._credit = new InformationLabel(3, {style_class: 'spotlit-wallpaper-info-credit'});
        for (const label of [this._title, this._description, this._credit]) {
            this._panel.add_child(label);
        }
        this._holder.add_child(this._panel);
        dialog.add_child(this._holder);

        this._connect(settings, 'changed::show-wallpaper-info', () => this._reload());
        this._connect(this._backgroundSettings, 'changed', () => this._reload());
        this._connect(dialog, 'notify::mapped', () => this._reload());
        this._connect(dialog._adjustment, 'notify::value', () => {
            this._syncVisibility();
            if (dialog._adjustment.value === 0)
                this._queueLayout();
        });
        this._connect(this._themeContext, 'changed', () => this._queueLayout());
        for (const actor of [this._holder, dialog._clock, dialog._notificationsBox]) {
            this._connect(actor, 'notify::allocation', () => this._queueLayout());
            this._connect(actor, 'notify::visible', () => this._queueLayout());
        }
        this._reload();
    }

    _connect(object, signal, callback) {
        this._signals.push([object, object.connect(signal, callback)]);
    }

    _reload() {
        const generation = ++this._generation;
        this._cancellable?.cancel();
        this._hasInfo = false;
        this._syncVisibility();
        if (!this._settings.get_boolean('show-wallpaper-info') ||
            !this._dialog.mapped || this._backgroundSettings.get_string('picture-options') === 'none')
            return;

        const uri = this._backgroundSettings.get_string('picture-uri');
        if (!uri)
            return;
        const image = Gio.File.new_for_commandline_arg(uri);
        if (image.get_uri_scheme() !== 'file')
            return;
        const file = Gio.File.new_for_path(`${image.get_path()}.spotlit.json`);
        const cancellable = new Gio.Cancellable();
        this._cancellable = cancellable;

        // Read a bounded local sidecar. Never fetch metadata from the lock screen.
        file.read_async(GLib.PRIORITY_DEFAULT, cancellable, (source, result) => {
            let stream;
            try {
                stream = source.read_finish(result);
            } catch {
                return;
            }
            stream.read_bytes_async(MAX_INFO_BYTES + 1, GLib.PRIORITY_DEFAULT, cancellable,
                (input, readResult) => {
                    try {
                        const bytes = input.read_bytes_finish(readResult);
                        if (this._destroyed || generation !== this._generation ||
                            bytes.get_size() > MAX_INFO_BYTES)
                            return;
                        const info = parseWallpaperInfo(
                            new TextDecoder('utf-8', {fatal: true}).decode(bytes.get_data()),
                            image.get_basename());
                        if (!info)
                            return;
                        this._title.text = info.title;
                        this._title.visible = !!info.title;
                        this._description.text = info.description;
                        this._description.visible = !!info.description;
                        this._credit.text = info.credit;
                        this._credit.visible = !!info.credit;
                        this._hasInfo = true;
                        this._queueLayout();
                    } catch {
                        // Missing or invalid information must not interrupt unlocking.
                    } finally {
                        input.close_async(GLib.PRIORITY_DEFAULT, null, (closed, closeResult) => {
                            try {
                                closed.close_finish(closeResult);
                            } catch {
                            }
                        });
                    }
                });
        });
    }

    _queueLayout() {
        if (this._destroyed || this._layoutId)
            return;
        this._layoutId = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            this._layoutId = 0;
            this._layout();
            return GLib.SOURCE_REMOVE;
        });
    }

    _layout() {
        const scale = this._themeContext.scale_factor;
        const margin = 32 * scale;
        const gap = 16 * scale;
        let width = Math.min(560 * scale, this._holder.width - margin * 2);
        this._fits = false;
        if (!this._hasInfo || width < 160 * scale) {
            this._syncVisibility();
            return;
        }

        const [originX, originY] = this._holder.get_transformed_position();
        const obstacles = [this._dialog._clock, this._dialog._notificationsBox]
            .filter(actor => actor.visible)
            .map(actor => {
                const [x, y] = actor.get_transformed_position();
                const [w, h] = actor.get_transformed_size();
                return {x: x - originX, y: y - originY, width: w, height: h};
            });
        let height = this._panel.get_preferred_height(width)[1];
        let y = this._holder.height - margin - height;
        const overlaps = obstacle => obstacle.width > 0 && obstacle.height > 0 &&
            margin < obstacle.x + obstacle.width + gap && margin + width + gap > obstacle.x &&
            y < obstacle.y + obstacle.height + gap && y + height + gap > obstacle.y;

        for (const obstacle of obstacles) {
            if (!overlaps(obstacle))
                continue;
            const spaceOnLeft = obstacle.x - margin - gap;
            if (spaceOnLeft >= 200 * scale) {
                width = Math.min(width, spaceOnLeft);
                height = this._panel.get_preferred_height(width)[1];
                y = this._holder.height - margin - height;
            } else {
                y = Math.min(y, obstacle.y - gap - height);
            }
        }
        this._fits = y >= margin && !obstacles.some(overlaps);
        this._panel.set_width(width);
        this._panel.set_position(margin, y);
        this._syncVisibility(true);
    }

    _syncVisibility(animate = false) {
        const progress = Math.max(0, Math.min(1, this._dialog._adjustment.value));
        const visible = this._hasInfo && this._fits && this._dialog.mapped && progress < 1;
        const wasVisible = this._panel.visible;
        this._panel.visible = visible;
        const opacity = visible ? Math.round(255 * (1 - progress)) : 0;
        this._panel.remove_transition('opacity');
        if (animate && visible && !wasVisible) {
            this._panel.opacity = 0;
            this._panel.ease({opacity, duration: 250, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
        } else {
            this._panel.opacity = opacity;
        }
    }

    destroy() {
        this._destroyed = true;
        this._cancellable?.cancel();
        if (this._layoutId)
            GLib.Source.remove(this._layoutId);
        this._layoutId = 0;
        for (const [object, id] of this._signals.splice(0))
            object.disconnect(id);
        this._holder.destroy();
    }
}
