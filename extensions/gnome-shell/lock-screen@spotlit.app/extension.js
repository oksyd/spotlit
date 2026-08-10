// SPDX-License-Identifier: Apache-2.0

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GDesktopEnums from 'gi://GDesktopEnums';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';
import UPower from 'gi://UPowerGlib';

import {
    Extension,
    InjectionManager,
} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {UnlockDialog} from 'resource:///org/gnome/shell/ui/unlockDialog.js';

const BACKGROUND_SCHEMA = 'org.gnome.desktop.background';
const INTERFACE_SCHEMA = 'org.gnome.desktop.interface';
const LOCK_SCREEN_SCHEMA = 'org.gnome.desktop.screensaver';
const CLEAR_MODE_CLASS = 'spotlit-clear-mode';
const PROMPT_CARD_CLASS = 'spotlit-unlock-prompt-card';
const KEEP_VISIBLE_INTERVAL_SECONDS = 10;

const BLUR_PRESETS = Object.freeze({
    soft: Object.freeze({brightness: 0.82, radius: 36}),
    clear: Object.freeze({brightness: 1.0, radius: 0}),
});

const DISPLAY_MODES = Object.freeze({
    SYSTEM: 'system',
    PLUGGED_IN: 'keep-on-ac',
    ALWAYS: 'keep-on',
});

class LockScreenDisplayController {
    constructor(settings) {
        this._settings = settings;
        this._screenShield = Main.screenShield;
        this._wakeIdleId = 0;
        this._keepVisibleId = 0;

        this._settingsChangedId = this._settings.connect(
            'changed::display-mode', () => this._sync());
        this._activeChangedId = this._screenShield.connect(
            'active-changed', () => this._sync());
        this._lockedChangedId = this._screenShield.connect(
            'locked-changed', () => this._sync());

        try {
            this._powerClient = UPower.Client.new();
            this._powerChangedIds = [
                this._powerClient.connect('notify::on-battery', () => this._sync()),
                this._powerClient.connect('notify::lid-is-closed', () => this._sync()),
            ];
        } catch (error) {
            console.warn(`Spotlit could not monitor power state: ${error.message}`);
            this._powerClient = null;
            this._powerChangedIds = [];
        }

        this._sync();
    }

    destroy() {
        this._stopKeepingVisible();

        if (this._settingsChangedId)
            this._settings.disconnect(this._settingsChangedId);
        if (this._activeChangedId)
            this._screenShield.disconnect(this._activeChangedId);
        if (this._lockedChangedId)
            this._screenShield.disconnect(this._lockedChangedId);
        for (const id of this._powerChangedIds)
            this._powerClient.disconnect(id);

        this._settingsChangedId = 0;
        this._activeChangedId = 0;
        this._lockedChangedId = 0;
        this._powerChangedIds = [];
        this._powerClient = null;
        this._screenShield = null;
        this._settings = null;
    }

    _sync() {
        if (!this._shouldKeepVisible()) {
            this._stopKeepingVisible();
            return;
        }

        if (this._wakeIdleId !== 0 || this._keepVisibleId !== 0)
            return;

        this._wakeIdleId = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            this._wakeIdleId = 0;
            if (this._shouldKeepVisible()) {
                this._screenShield._wakeUpScreen();
                this._startKeepingVisible();
            }
            return GLib.SOURCE_REMOVE;
        });
        GLib.Source.set_name_by_id(
            this._wakeIdleId, '[spotlit] wake lock screen display');
    }

    _shouldKeepVisible() {
        if (!this._screenShield?.active || !this._screenShield.locked)
            return false;
        if (this._powerClient?.lid_is_closed)
            return false;

        const mode = this._settings.get_string('display-mode');
        if (mode === DISPLAY_MODES.ALWAYS)
            return true;
        if (mode === DISPLAY_MODES.PLUGGED_IN)
            return this._powerClient !== null && !this._powerClient.on_battery;
        return false;
    }

    _startKeepingVisible() {
        if (this._keepVisibleId !== 0)
            return;

        this._keepVisibleId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT,
            KEEP_VISIBLE_INTERVAL_SECONDS,
            () => {
                if (!this._shouldKeepVisible()) {
                    this._keepVisibleId = 0;
                    return GLib.SOURCE_REMOVE;
                }

                this._screenShield.emit('wake-up-screen');
                return GLib.SOURCE_CONTINUE;
            });
        GLib.Source.set_name_by_id(
            this._keepVisibleId, '[spotlit] keep lock screen display visible');
    }

    _stopKeepingVisible() {
        if (this._wakeIdleId !== 0) {
            GLib.Source.remove(this._wakeIdleId);
            this._wakeIdleId = 0;
        }
        if (this._keepVisibleId !== 0) {
            GLib.Source.remove(this._keepVisibleId);
            this._keepVisibleId = 0;
        }
    }
}

class LockScreenBackgroundManager {
    constructor(container, monitorIndex) {
        this._lockSettings = new Gio.Settings({schema_id: LOCK_SCREEN_SCHEMA});
        this._desktopSettings = new Gio.Settings({schema_id: BACKGROUND_SCHEMA});
        this._interfaceSettings = new Gio.Settings({schema_id: INTERFACE_SCHEMA});
        this._background = new Meta.Background({meta_display: global.display});
        this._actor = new Meta.BackgroundActor({
            meta_display: global.display,
            monitor: monitorIndex,
            request_mode: Clutter.RequestMode.CONTENT_SIZE,
        });
        this._actor.content.set({background: this._background});
        container.add_child(this._actor);

        this._lockSettings.connectObject('changed', () => this._apply(), this._actor);
        this._desktopSettings.connectObject('changed', () => this._apply(), this._actor);
        this._interfaceSettings.connectObject(
            'changed::color-scheme', () => this._apply(), this._actor);
        this._apply();
    }

    _apply() {
        const primary = this._lockSettings.get_string('primary-color');
        const secondary = this._lockSettings.get_string('secondary-color');
        const [, primaryColor] = Cogl.Color.from_string(primary);
        const [, secondaryColor] = Cogl.Color.from_string(secondary);
        const shading = this._lockSettings.get_enum('color-shading-type');

        if (shading === GDesktopEnums.BackgroundShading.SOLID)
            this._background.set_color(primaryColor);
        else
            this._background.set_gradient(shading, primaryColor, secondaryColor);

        const style = this._lockSettings.get_enum('picture-options');
        const uri = this._lockSettings.get_string('picture-uri') || this._desktopUri();
        const file = uri && style !== GDesktopEnums.BackgroundStyle.NONE
            ? Gio.File.new_for_commandline_arg(uri)
            : null;
        this._background.set_file(file, style);
    }

    _desktopUri() {
        const colorScheme = this._interfaceSettings.get_enum('color-scheme');
        const key = colorScheme === GDesktopEnums.ColorScheme.PREFER_DARK
            ? 'picture-uri-dark'
            : 'picture-uri';
        return this._desktopSettings.get_string(key);
    }

    destroy() {
        this._lockSettings?.disconnectObject(this._actor);
        this._desktopSettings?.disconnectObject(this._actor);
        this._interfaceSettings?.disconnectObject(this._actor);
        this._actor?.destroy();

        this._actor = null;
        this._background = null;
        this._lockSettings = null;
        this._desktopSettings = null;
        this._interfaceSettings = null;
    }
}

export default class SpotlitLockScreenExtension extends Extension {
    enable() {
        const schemaSource = Gio.SettingsSchemaSource.get_default();
        if (!schemaSource.lookup(LOCK_SCREEN_SCHEMA, true))
            throw new Error(`${LOCK_SCREEN_SCHEMA} is unavailable`);

        this._settings = this.getSettings();
        this._dialogs = new Map();
        this._injectionManager = new InjectionManager();
        this._displayController = new LockScreenDisplayController(this._settings);

        this._injectionManager.overrideMethod(
            UnlockDialog.prototype,
            '_createBackground',
            () => {
                const extension = this;
                return function (monitorIndex) {
                    extension._createBackground(this, monitorIndex);
                };
            });

        this._injectionManager.overrideMethod(
            UnlockDialog.prototype,
            '_updateBackgroundEffects',
            originalMethod => {
                const extension = this;
                return function () {
                    extension._trackDialog(this);
                    extension._updateBackgroundEffects(this, originalMethod);
                };
            });

        this._injectionManager.overrideMethod(
            UnlockDialog.prototype,
            '_ensureAuthPrompt',
            originalMethod => {
                const extension = this;
                return function (...args) {
                    const result = originalMethod.apply(this, args);
                    extension._updatePromptCard(this);
                    return result;
                };
            });

        this._settingsChangedId = this._settings.connect(
            'changed::blur-mode', () => this._refreshDialogs());
    }

    disable() {
        // unlock-dialog is required to keep the wallpaper and contrast treatment
        // active while the user session is locked.
        this._displayController?.destroy();
        this._displayController = null;

        if (this._settingsChangedId) {
            this._settings.disconnect(this._settingsChangedId);
            this._settingsChangedId = null;
        }

        this._injectionManager?.clear();
        this._injectionManager = null;

        for (const [dialog, destroyId] of this._dialogs ?? []) {
            dialog.disconnect(destroyId);
            dialog.remove_style_class_name(CLEAR_MODE_CLASS);
            dialog._promptBox?.remove_style_class_name(PROMPT_CARD_CLASS);
            dialog._updateBackgrounds();
        }

        this._dialogs?.clear();
        this._dialogs = null;
        this._settings = null;
    }

    _createBackground(dialog, monitorIndex) {
        const monitor = Main.layoutManager.monitors[monitorIndex];
        const widget = new St.Widget({
            style_class: 'screen-shield-background',
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
            effect: new Shell.BlurEffect({name: 'blur'}),
        });
        const backgroundManager = new LockScreenBackgroundManager(widget, monitorIndex);

        dialog._bgManagers.push(backgroundManager);
        dialog._backgroundGroup.add_child(widget);
    }

    _trackDialog(dialog) {
        if (this._dialogs.has(dialog))
            return;

        const destroyId = dialog.connect('destroy', () => {
            this._dialogs.delete(dialog);
        });
        this._dialogs.set(dialog, destroyId);
    }

    _updateBackgroundEffects(dialog, originalMethod) {
        const mode = this._settings.get_string('blur-mode');
        if (mode === 'clear')
            dialog.add_style_class_name(CLEAR_MODE_CLASS);
        else
            dialog.remove_style_class_name(CLEAR_MODE_CLASS);

        this._updatePromptCard(dialog);

        const preset = BLUR_PRESETS[mode];
        if (!preset) {
            originalMethod.call(dialog);
            return;
        }

        const themeContext = St.ThemeContext.get_for_stage(global.stage);
        for (const widget of dialog._backgroundGroup) {
            widget.get_effect('blur')?.set({
                brightness: preset.brightness,
                radius: preset.radius * themeContext.scale_factor,
            });
        }
    }

    _updatePromptCard(dialog) {
        if (this._settings.get_string('blur-mode') === 'clear')
            dialog._promptBox?.add_style_class_name(PROMPT_CARD_CLASS);
        else
            dialog._promptBox?.remove_style_class_name(PROMPT_CARD_CLASS);
    }

    _refreshDialogs() {
        for (const dialog of this._dialogs.keys())
            dialog._updateBackgroundEffects();
    }
}
