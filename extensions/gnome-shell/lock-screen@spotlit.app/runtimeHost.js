// SPDX-License-Identifier: Apache-2.0

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import * as Config from 'resource:///org/gnome/shell/misc/config.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const SOURCE = Gio.File.new_for_uri(import.meta.url).get_parent();
const BOOTSTRAP_FILES = ['extension.js', 'runtimeHost.js'];
const PAYLOAD_FILES = [
    'metadata.json', 'runtime.js', 'wallpaperInfo.js', 'runtime.css',
    'schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml',
    'schemas/gschemas.compiled',
];
const MAX_MODULE_REVISIONS = 8;
const importedRevisions = new Set();
const BUS_PATH = '/io/github/oksyd/Spotlit/LockScreen';
const BUS_INTERFACE = `<node><interface name="io.github.oksyd.Spotlit.LockScreen1">
    <method name="GetStatus"><arg type="s" direction="out"/></method>
    <method name="Reload"><arg type="s" direction="out"/></method>
</interface></node>`;

function readFiles(directory, names) {
    return names.map(name => {
        const file = directory.resolve_relative_path(name);
        const info = file.query_info('standard::size,standard::type',
            Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS, null);
        if (info.get_file_type() !== Gio.FileType.REGULAR || info.get_size() > 1024 * 1024)
            throw new Error(`Invalid runtime file: ${name}`);
        const [, contents] = file.load_contents(null);
        return {name, contents};
    });
}

function digest(files) {
    const checksum = new GLib.Checksum(GLib.ChecksumType.SHA256);
    const encoder = new TextEncoder();
    for (const {name, contents} of files) {
        checksum.update(encoder.encode(`${name}\0${contents.length}\0`));
        checksum.update(contents);
    }
    return checksum.get_string();
}

// Capture the loader actually imported by this Shell, not later files on disk.
const BOOTSTRAP_REVISION = digest(readFiles(SOURCE, BOOTSTRAP_FILES));

export class RuntimeHost {
    constructor(extension) {
        this._extension = extension;
        this._runtime = null;
        this._lastGood = null;
        this._enabled = false;
        this._busy = false;
        this._pending = false;
        this._generation = 0;
        this._signals = [];
        this._idleWaiters = [];
        this._result = 'inactive';
    }

    async enable() {
        this._enabled = true;
        const generation = ++this._generation;
        if (this._busy)
            await new Promise(resolve => this._idleWaiters.push(resolve));
        if (!this._enabled || generation !== this._generation)
            return;
        const reply = await this.reload();
        if (!this._enabled || generation !== this._generation)
            return;
        if (!reply.active && this._lastGood)
            this._activate(this._lastGood);
        if (!this._runtime) {
            this.disable();
            throw new Error(`Spotlit runtime could not start: ${reply.result}`);
        }
        this._bus = Gio.DBusExportedObject.wrapJSObject(BUS_INTERFACE, {
            GetStatus: () => JSON.stringify(this.status()),
            ReloadAsync: (_parameters, invocation) => {
                this.reload().then(reply => invocation.return_value(
                    new GLib.Variant('(s)', [JSON.stringify(reply)]))).catch(error => {
                    console.error(`Spotlit runtime reload failed: ${error.message}`);
                    invocation.return_value(new GLib.Variant('(s)',
                        [JSON.stringify(this._reply('failed', false))]));
                });
            },
        });
        this._bus.export(Gio.DBus.session, BUS_PATH);
        if (Main.screenShield) {
            for (const signal of ['locked-changed', 'active-changed']) {
                const id = Main.screenShield.connect(signal, () => this._resumePending());
                this._signals.push([Main.screenShield, id]);
            }
        }
    }

    disable() {
        this._enabled = false;
        this._pending = false;
        ++this._generation;
        this._bus?.unexport();
        this._bus = null;
        for (const [object, id] of this._signals.splice(0))
            object.disconnect(id);
        const previous = this._runtime;
        this._runtime = null;
        try {
            previous?.instance.disable();
        } finally {
            if (previous)
                this._theme().unload_stylesheet(previous.stylesheet);
        }
        this._result = 'inactive';
    }

    status() {
        try {
            const payload = this._payload();
            const current = payload.compatible && this._runtime?.revision === payload.revision;
            const result = current ? 'ready' : !payload.compatible ? 'restart-required'
                : this._result === 'ready' ? 'pending' : this._result;
            return this._reply(result, current);
        } catch {
            return this._reply('failed', false);
        }
    }

    async reload() {
        if (!this._enabled)
            return this._reply('inactive', false);
        if (this._busy)
            return this._reply('busy', false);
        this._busy = true;
        const generation = this._generation;
        try {
            const payload = this._payload();
            if (!payload.compatible)
                return this._finish('restart-required');
            if (this._runtime?.revision === payload.revision)
                return this._finish('ready', true);
            // Starting while locked is normal; replacing a working runtime is not.
            if (this._runtime && this._isLocked()) {
                this._pending = true;
                return this._finish('pending');
            }
            if (!importedRevisions.has(payload.revision) && importedRevisions.size >= MAX_MODULE_REVISIONS)
                return this._finish('restart-required');
            const directory = this._stage(payload);
            importedRevisions.add(payload.revision);
            const module = await import(directory.get_child('runtime.js').get_uri());
            if (!this._enabled || generation !== this._generation)
                return this._reply('inactive', false);
            if (module.RUNTIME_API !== 1)
                return this._finish('restart-required');
            const latest = this._payload();
            if (!latest.compatible)
                return this._finish('restart-required');
            if (latest.revision !== payload.revision) {
                this._pending = true;
                return this._finish('pending');
            }
            if (this._runtime && this._isLocked()) {
                this._pending = true;
                return this._finish('pending');
            }
            const candidate = {
                revision: payload.revision,
                metadata: payload.metadata,
                stylesheet: directory.get_child('runtime.css'),
                instance: new module.default({...payload.metadata, dir: directory, path: directory.get_path()}),
            };
            this._activate(candidate);
            return this._finish('ready', true);
        } catch (error) {
            console.error(`Spotlit runtime update failed: ${error.message}`);
            return this._finish('failed');
        } finally {
            this._busy = false;
            for (const resolve of this._idleWaiters.splice(0))
                resolve();
            this._resumePending();
        }
    }

    _activate(candidate) {
        // Runtime API 1 requires synchronous enable/disable for atomic rollback.
        const previous = this._runtime;
        const theme = this._theme();
        theme.load_stylesheet(candidate.stylesheet);
        try {
            previous?.instance.disable();
            candidate.instance.enable();
        } catch (error) {
            try {
                candidate.instance.disable();
            } catch (cleanupError) {
                console.error(`Spotlit candidate cleanup failed: ${cleanupError.message}`);
            }
            theme.unload_stylesheet(candidate.stylesheet);
            try {
                previous?.instance.enable();
            } catch (rollbackError) {
                this._runtime = null;
                try {
                    previous.instance.disable();
                } finally {
                    theme.unload_stylesheet(previous.stylesheet);
                }
                throw new Error(`Runtime rollback failed: ${rollbackError.message}`);
            }
            throw error;
        }
        this._runtime = candidate;
        this._lastGood = candidate;
        if (previous)
            theme.unload_stylesheet(previous.stylesheet);
        this._extension.metadata = {...candidate.metadata, dir: SOURCE, path: SOURCE.get_path()};
        const record = Main.extensionManager.lookup(this._extension.uuid);
        if (record?.stateObj === this._extension) {
            record.metadata = candidate.metadata;
            Main.extensionManager.emit('extension-state-changed', record);
        }
    }

    _payload() {
        const files = readFiles(SOURCE, PAYLOAD_FILES);
        const metadata = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(files[0].contents));
        const major = Config.PACKAGE_VERSION.split('.')[0];
        const compatible = metadata.uuid === this._extension.uuid &&
            metadata['spotlit-runtime-api'] === 1 &&
            metadata['shell-version']?.includes(major) &&
            digest(readFiles(SOURCE, BOOTSTRAP_FILES)) === BOOTSTRAP_REVISION;
        return {files, metadata, compatible, revision: digest(files)};
    }

    _stage(payload) {
        const processId = new Gio.Credentials().get_unix_pid();
        const path = GLib.build_filenamev([GLib.get_user_runtime_dir(),
            `spotlit-shell-${processId}`, `spotlit-${payload.revision}`]);
        GLib.mkdir_with_parents(path, 0o700);
        const directory = Gio.File.new_for_path(path);
        for (const {name, contents} of payload.files) {
            const file = directory.resolve_relative_path(name);
            GLib.mkdir_with_parents(file.get_parent().get_path(), 0o700);
            if (file.query_exists(null)) {
                const [, existing] = file.load_contents(null);
                if (!new GLib.Bytes(existing).equal(new GLib.Bytes(contents)))
                    throw new Error('Cached runtime is not immutable');
            } else {
                file.replace_contents(contents, null, false, Gio.FileCreateFlags.PRIVATE, null);
            }
        }
        return directory;
    }

    _reply(result, current) {
        return {protocol: 1, result, active: !!this._runtime, current: !!current,
            revision: this._runtime?.revision ?? ''};
    }

    _finish(result, current = false) {
        this._result = result;
        if (result !== 'pending')
            this._pending = false;
        return this._reply(result, current);
    }

    _isLocked() {
        return !!(Main.screenShield?.locked || Main.screenShield?.active);
    }

    _resumePending() {
        if (this._enabled && this._pending && !this._busy && !this._isLocked()) {
            this._pending = false;
            this.reload().catch(error => console.error(error));
        }
    }

    _theme() {
        return St.ThemeContext.get_for_stage(global.stage).get_theme();
    }
}
