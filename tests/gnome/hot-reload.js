import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {UnlockDialog} from 'resource:///org/gnome/shell/ui/unlockDialog.js';

export const METRICS = {};

function assert(value, message) {
    if (!value)
        throw new Error(message);
}

function delay(ms = 100) {
    return new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
        resolve();
        return GLib.SOURCE_REMOVE;
    }));
}

async function until(predicate, message) {
    for (let i = 0; i < 50; i++) {
        if (predicate())
            return;
        await delay();
    }
    throw new Error(message);
}

function read(file) {
    return new TextDecoder().decode(file.load_contents(null)[1]);
}

function write(file, text) {
    file.replace_contents(text, null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
}

async function runCommand(arguments_) {
    const process = Gio.Subprocess.new(arguments_,
        Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE);
    const [, stdout, stderr] = await new Promise((resolve, reject) => {
        process.communicate_utf8_async(null, null, (source, result) => {
            try {
                resolve(source.communicate_utf8_finish(result));
            } catch (error) {
                reject(error);
            }
        });
    });
    assert(process.get_successful(), stderr);
    return stdout;
}

async function call(method) {
    const stdout = await runCommand(['gdbus', 'call', '--session', '--timeout', '5',
        '--dest', 'org.gnome.Shell', '--object-path', '/io/github/oksyd/Spotlit/LockScreen',
        '--method', `io.github.oksyd.Spotlit.LockScreen1.${method}`]);
    const match = stdout.trim().match(/^\('(.*)',\)$/);
    assert(match, `unexpected D-Bus response: ${stdout}`);
    return JSON.parse(match[1]);
}

export async function run() {
    assert(GLib.getenv('GSETTINGS_BACKEND') === 'keyfile', 'requires isolated settings');
    assert(GLib.getenv('DBUS_SYSTEM_BUS_ADDRESS') === GLib.getenv('DBUS_SESSION_BUS_ADDRESS'),
        'requires isolated system bus');
    const uuid = 'lock-screen@spotlit.app';
    await until(() => Main.extensionManager.lookup(uuid)?.stateObj?._host?._bus, 'runtime host did not start');
    const extension = Main.extensionManager.lookup(uuid);
    assert(extension.path.startsWith(GLib.getenv('XDG_DATA_HOME')), 'requires isolated extension');
    const wrapper = extension.stateObj;
    const host = wrapper._host;
    const directory = Gio.File.new_for_path(extension.path);
    const infoFile = directory.get_child('wallpaperInfo.js');
    const runtimeFile = directory.get_child('runtime.js');
    const loaderFile = directory.get_child('runtimeHost.js');
    const cssFile = directory.get_child('runtime.css');
    const schemaFile = directory.resolve_relative_path('schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml');
    const infoSource = read(infoFile);
    const runtimeSource = read(runtimeFile);
    const loaderSource = read(loaderFile);
    const cssSource = read(cssFile);
    const schemaSource = read(schemaFile);
    const originalStatus = await call('GetStatus');
    assert(originalStatus.active && originalStatus.current && originalStatus.protocol === 1,
        'startup status must acknowledge the running code');
    assert((await call('Reload')).revision === originalStatus.revision, 'unchanged reload must be a no-op');

    const image = Gio.File.new_for_path(`${GLib.getenv('XDG_RUNTIME_DIR')}/hot-reload.jpg`);
    Gio.File.new_for_path(GLib.getenv('SPOTLIT_TEST_WALLPAPER')).copy(image, Gio.FileCopyFlags.NONE, null, null);
    write(Gio.File.new_for_path(`${image.get_path()}.spotlit.json`), JSON.stringify({
        version: 1, image: image.get_basename(), wallpaper_id: 'hot-reload',
        title: 'A long wallpaper title '.repeat(9),
    }));
    const settings = wrapper.getSettings();
    settings.set_boolean('show-wallpaper-info', true);
    const background = new Gio.Settings({schema_id: 'org.gnome.desktop.screensaver'});
    background.set_string('picture-uri', image.get_uri());
    background.set_string('picture-options', 'zoom');
    Main.overview.hide();
    const parent = new St.Widget({layout_manager: new Clutter.BinLayout()});
    parent.add_constraint(new Clutter.BindConstraint({source: global.stage, coordinate: Clutter.BindCoordinate.SIZE}));
    Main.layoutManager.uiGroup.add_child(parent);
    const dialog = new UnlockDialog(parent);
    dialog.add_constraint(new Clutter.BindConstraint({source: parent, coordinate: Clutter.BindCoordinate.SIZE}));
    dialog.show();
    const getInfo = () => host._runtime?.instance._dialogs.get(dialog)?.info;
    await until(() => getInfo()?._panel.visible, 'original information did not appear');
    await delay(300);
    const oldInfo = getInfo();
    const oldFontSize = oldInfo._title.get_theme_node().get_font().get_size();
    assert(oldInfo._title.clutter_text.get_layout().get_line_count() === 2, 'original runtime must use two lines');

    write(infoFile, infoSource.replace('new InformationLabel(2,', 'new InformationLabel(3,'));
    write(cssFile, cssSource.replace('font-size: 1.2em;', 'font-size: 1.25em;'));
    write(schemaFile, schemaSource.replace('</schema>',
        '<key name="hot-reload-test" type="s"><default>\'new schema\'</default></key></schema>'));
    const bundleDirectory = `${GLib.getenv('XDG_RUNTIME_DIR')}/bundles`;
    GLib.mkdir_with_parents(bundleDirectory, 0o700);
    await runCommand(['gnome-extensions', 'pack', '--force', '--out-dir', bundleDirectory,
        '--schema', `${extension.path}/schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml`,
        '--extra-source', 'runtimeHost.js', '--extra-source', 'runtime.js',
        '--extra-source', 'runtime.css', '--extra-source', 'wallpaperInfo.js', extension.path]);
    write(infoFile, infoSource);
    write(cssFile, cssSource);
    write(schemaFile, schemaSource);
    assert((await call('GetStatus')).current, 'building a bundle must not change the running revision');
    await runCommand(['gnome-extensions', 'install', '--force',
        `${bundleDirectory}/${uuid}.shell-extension.zip`]);
    assert(!(await call('GetStatus')).current, 'changed payload must be reported without executing it');
    assert(getInfo() === oldInfo, 'querying status must not reload code');
    const updated = await call('Reload');
    assert(updated.current && updated.active && updated.revision !== originalStatus.revision, 'D-Bus reload must apply new code');
    assert(extension.stateObj === wrapper, 'Shell-managed bootstrap must remain stable');
    assert(oldInfo._destroyed && oldInfo._signals.length === 0, 'old information must be fully disconnected');
    dialog._updateBackgroundEffects();
    await until(() => getInfo()?._panel.visible, 'updated information did not appear');
    await delay(300);
    assert(getInfo()._title.clutter_text.get_layout().get_line_count() === 3, 'new relative dependency must actually run');
    assert(getInfo()._title.get_theme_node().get_font().get_size() > oldFontSize,
        'the updated stylesheet must actually apply');
    assert(host._runtime.instance.getSettings().get_string('hot-reload-test') === 'new schema',
        'the updated compiled schema must actually load');
    const good = host._runtime;

    write(runtimeFile, runtimeSource.replace('this._settings = this.getSettings();',
        "this._settings = this.getSettings(); throw new Error('injected failure');"));
    const failed = await call('Reload');
    assert(failed.result === 'failed' && failed.active && !failed.current, 'failed update must report rollback');
    assert(host._runtime === good, 'failed update must retain the previous revision');
    dialog._updateBackgroundEffects();
    await until(() => getInfo()?._panel.visible, 'rolled-back runtime must still work');
    write(runtimeFile, runtimeSource);

    const originalIsLocked = host._isLocked.bind(host);
    host._isLocked = () => true;
    write(infoFile, infoSource.replace('new InformationLabel(2,', 'new InformationLabel(4,'));
    const pending = await call('Reload');
    assert(pending.result === 'pending' && host._runtime === good, 'locked updates must be deferred');
    host._isLocked = () => false;
    host._resumePending();
    await until(() => host.status().current, 'deferred update must resume when unlocked');
    host._isLocked = originalIsLocked;
    dialog._updateBackgroundEffects();
    await until(() => getInfo()?._panel.visible, 'deferred runtime did not appear');
    assert(getInfo()._title._maxLines === 4, 'deferred update must execute new dependency');

    write(loaderFile, `${loaderSource}\n// incompatible bootstrap change\n`);
    assert((await call('Reload')).result === 'restart-required', 'bootstrap changes must not be hot-loaded');
    write(loaderFile, loaderSource);

    const slowSource = `${runtimeSource}\nawait new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, 400, () => { resolve(); return GLib.SOURCE_REMOVE; }));\n`;
    write(runtimeFile, `${slowSource}// superseded import\n`);
    const superseded = call('Reload');
    await until(() => host._busy, 'superseded import did not start');
    write(runtimeFile, runtimeSource);
    assert(!(await superseded).current, 'superseded imports must not acknowledge outdated files');
    await until(() => !host._busy && host.status().current, 'the newest revision must take over');

    write(runtimeFile, slowSource);
    const interrupted = call('Reload');
    await until(() => host._busy, 'slow import did not start');
    assert((await call('Reload')).result === 'busy', 'concurrent requests must not overlap');
    await Main.extensionManager._callExtensionDisable(uuid);
    await Main.extensionManager._callExtensionEnable(uuid);
    assert((await interrupted).result === 'inactive', 'disabled generations must not activate late');
    assert((await call('GetStatus')).active, 'rapid disable and enable must recover');
    write(runtimeFile, runtimeSource);
    await call('Reload');

    let limited = false;
    for (let i = 0; i < 10; i++) {
        write(cssFile, `${cssSource}\n/* revision ${i} */\n`);
        const reply = await call('Reload');
        if (reply.result === 'restart-required') {
            limited = true;
            assert(reply.active, 'reload limit must keep the last working runtime');
            break;
        }
        assert(reply.current, 'revision within budget must load');
    }
    assert(limited, 'module imports must have a per-session bound');
    await Main.extensionManager._callExtensionDisable(uuid);
    await Main.extensionManager._callExtensionEnable(uuid);
    assert((await call('GetStatus')).active, 'reload limit must not prevent re-enabling the last good runtime');
    assert(!global.context.unsafe_mode, 'hot reload must never enable unsafe mode');
    dialog.destroy();
    parent.destroy();
    print('Spotlit D-Bus hot reload checks passed');
}
