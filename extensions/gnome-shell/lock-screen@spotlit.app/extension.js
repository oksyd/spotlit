// SPDX-License-Identifier: Apache-2.0

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {RuntimeHost} from './runtimeHost.js';

export default class SpotlitLockScreenExtension extends Extension {
    async enable() {
        this._host ??= new RuntimeHost(this);
        await this._host.enable();
    }

    disable() {
        this._host?.disable();
    }
}
