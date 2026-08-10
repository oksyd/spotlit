// SPDX-License-Identifier: Apache-2.0

import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const BLUR_MODES = Object.freeze(['system', 'soft', 'clear']);
const DISPLAY_MODES = Object.freeze(['system', 'keep-on-ac', 'keep-on']);

export default class SpotlitLockScreenPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        const page = new Adw.PreferencesPage({
            title: 'Lock Screen',
            icon_name: 'preferences-desktop-wallpaper-symbolic',
        });
        const group = new Adw.PreferencesGroup({title: 'Appearance'});
        const blurRow = new Adw.ComboRow({
            title: 'Background Blur',
            model: Gtk.StringList.new(['System Default', 'Soft', 'Clear']),
            selected: this._modeIndex(BLUR_MODES, settings.get_string('blur-mode')),
        });
        const behaviorGroup = new Adw.PreferencesGroup({title: 'Display'});
        const displayRow = new Adw.ComboRow({
            title: 'Lock Screen Display',
            model: Gtk.StringList.new([
                'System Default',
                'Keep On When Plugged In',
                'Always Keep On',
            ]),
            selected: this._modeIndex(
                DISPLAY_MODES, settings.get_string('display-mode')),
        });

        blurRow.connect('notify::selected', () => {
            settings.set_string('blur-mode', BLUR_MODES[blurRow.selected] ?? 'system');
        });
        displayRow.connect('notify::selected', () => {
            settings.set_string(
                'display-mode', DISPLAY_MODES[displayRow.selected] ?? 'system');
        });
        const settingsChangedIds = [
            settings.connect('changed::blur-mode', () => {
                blurRow.selected = this._modeIndex(
                    BLUR_MODES, settings.get_string('blur-mode'));
            }),
            settings.connect('changed::display-mode', () => {
                displayRow.selected = this._modeIndex(
                    DISPLAY_MODES, settings.get_string('display-mode'));
            }),
        ];
        window.connect('close-request', () => {
            for (const id of settingsChangedIds.splice(0))
                settings.disconnect(id);
            return false;
        });

        group.add(blurRow);
        behaviorGroup.add(displayRow);
        page.add(group);
        page.add(behaviorGroup);
        window.add(page);
    }

    _modeIndex(modes, mode) {
        const index = modes.indexOf(mode);
        return index < 0 ? 0 : index;
    }
}
