// Run with gnome-shell-test-tool --headless --extension <bundle> in a private D-Bus session.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {UnlockDialog} from 'resource:///org/gnome/shell/ui/unlockDialog.js';

export const METRICS = {};

function assert(condition, message) {
    if (!condition)
        throw new Error(message);
}

function delay(milliseconds = 300) {
    return new Promise(resolve => {
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, milliseconds, () => {
            resolve();
            return GLib.SOURCE_REMOVE;
        });
    });
}

async function until(predicate, message) {
    for (let attempt = 0; attempt < 50; attempt++) {
        if (predicate())
            return;
        await delay(100);
    }
    throw new Error(message);
}

export async function run() {
    assert(GLib.getenv('GSETTINGS_BACKEND') === 'keyfile', 'requires isolated test settings');
    const output = GLib.getenv('SPOTLIT_TEST_OUTPUT');
    assert(output, 'SPOTLIT_TEST_OUTPUT must point to an isolated test directory');
    GLib.mkdir_with_parents(output, 0o700);
    await until(() => Main.extensionManager.lookup('lock-screen@spotlit.app')?.stateObj,
        'packaged extension did not load');
    const extension = Main.extensionManager.lookup('lock-screen@spotlit.app');
    assert(extension?.stateObj, 'packaged extension must load');
    await until(() => extension.stateObj._host?._runtime, 'runtime did not start');
    const runtime = extension.stateObj._host._runtime.instance;
    const settings = extension.stateObj.getSettings();
    assert(!settings.get_boolean('show-wallpaper-info'), 'information must default off');
    const {parseWallpaperInfo} = await import(
        Gio.File.new_for_path(`${extension.path}/wallpaperInfo.js`).get_uri());

    const photo = GLib.getenv('SPOTLIT_TEST_WALLPAPER');
    assert(photo, 'SPOTLIT_TEST_WALLPAPER must point to a test image');
    const image = Gio.File.new_for_path(`${output}/landscape.jpg`);
    Gio.File.new_for_path(photo).copy(image, Gio.FileCopyFlags.OVERWRITE, null, null);
    const data = {
        version: 1,
        image: image.get_basename(),
        wallpaper_id: 'test-landscape',
        title: 'Bluebells in the morning light',
        description: 'A quiet woodland in spring',
        credit: 'Photograph: mendhak',
    };
    const infoFile = Gio.File.new_for_path(`${image.get_path()}.spotlit.json`);
    const writeInfo = value => infoFile.replace_contents(
        JSON.stringify(value), null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
    writeInfo(data);
    assert(parseWallpaperInfo(JSON.stringify({...data, image: 'wrong.jpg'}), data.image) === null,
        'must reject metadata for another image');
    assert(parseWallpaperInfo(JSON.stringify({...data, title: null, description: null, credit: null}), data.image) === null,
        'must hide missing metadata');
    assert(parseWallpaperInfo(JSON.stringify({...data, description: data.title}), data.image).description === '',
        'must remove duplicate descriptions');

    const background = new Gio.Settings({schema_id: 'org.gnome.desktop.screensaver'});
    background.set_string('picture-uri', image.get_uri());
    background.set_string('picture-options', 'zoom');
    settings.set_string('blur-mode', 'clear');
    Main.overview.hide();
    const parent = new St.Widget({layout_manager: new Clutter.BinLayout()});
    parent.add_constraint(new Clutter.BindConstraint({source: global.stage, coordinate: Clutter.BindCoordinate.SIZE}));
    Main.layoutManager.uiGroup.add_child(parent);
    const dialog = new UnlockDialog(parent);
    dialog.add_constraint(new Clutter.BindConstraint({source: parent, coordinate: Clutter.BindCoordinate.SIZE}));
    dialog.show();
    await until(() => runtime._dialogs.get(dialog)?.info, 'information controller did not attach');
    const info = runtime._dialogs.get(dialog).info;
    assert(!info._panel.visible, 'disabled information must stay hidden');
    settings.set_boolean('show-wallpaper-info', true);
    await until(() => info._panel.visible, 'information did not appear');
    await delay();
    assert(info._title.text === data.title, 'title must match the applied wallpaper');
    assert(info._credit.text === data.credit && info._credit.visible,
        'credit must stay visible independently of the description');
    assert(!info._panel.reactive && !info._panel.can_focus, 'information must not consume input');
    assert(info._panel.get_parent() !== dialog._backgroundGroup, 'text must not be blurred');

    const capture = async name => {
        const file = Gio.File.new_for_path(`${output}/${name}.png`);
        const stream = file.replace(null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
        await new Shell.Screenshot().screenshot(false, stream);
        stream.close(null);
    };
    await capture('lock-screen-info');

    dialog._adjustment.value = 0.5;
    await delay();
    assert(info._panel.opacity > 0 && info._panel.opacity < 255, 'information must fade during unlocking');
    dialog._adjustment.value = 1;
    await delay();
    assert(!info._panel.visible, 'information must hide during authentication');
    dialog._adjustment.value = 0;
    await until(() => info._panel.visible, 'information must return with the clock');

    const notification = new St.Label({
        text: 'Test notification',
        style: 'background-color: rgba(30, 30, 30, 0.9); padding: 32px; min-width: 480px;',
    });
    dialog._notificationsBox.add_child(notification);
    dialog._notificationsBox.show();
    await delay();
    const [x, y] = info._panel.get_transformed_position();
    const [nx, ny] = notification.get_transformed_position();
    const [nw, nh] = notification.get_transformed_size();
    assert(!info._panel.visible || x + info._panel.width <= nx || y + info._panel.height <= ny ||
        x >= nx + nw || y >= ny + nh, 'information must not overlap notifications');
    await capture('lock-screen-info-notification');
    notification.destroy();

    background.set_string('picture-uri', Gio.File.new_for_path(`${output}/missing.jpg`).get_uri());
    await delay();
    assert(!info._panel.visible, 'changing the image must clear the old caption immediately');
    writeInfo({...data, image: 'wrong.jpg'});
    background.set_string('picture-uri', image.get_uri());
    await delay();
    assert(!info._panel.visible, 'mismatched metadata must not display');
    infoFile.replace_contents('{invalid', null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
    settings.set_boolean('show-wallpaper-info', false);
    await delay();
    settings.set_boolean('show-wallpaper-info', true);
    await delay();
    assert(!info._panel.visible, 'malformed metadata must not display');

    writeInfo({...data, title: 'x'.repeat(20 * 1024)});
    settings.set_boolean('show-wallpaper-info', false);
    await delay();
    settings.set_boolean('show-wallpaper-info', true);
    await delay();
    assert(!info._panel.visible, 'oversized metadata must not display');

    writeInfo({
        ...data,
        title: 'A long landscape title '.repeat(15),
        description: 'Location '.repeat(100),
        credit: 'A detailed location and photographic credit '.repeat(8),
    });
    settings.set_boolean('show-wallpaper-info', false);
    await delay();
    settings.set_boolean('show-wallpaper-info', true);
    await until(() => info._panel.visible, 'long metadata must be bounded, not discarded');
    await delay();
    assert(info._title.clutter_text.get_layout().is_ellipsized(), 'long titles must be ellipsized');
    assert(info._title.clutter_text.get_layout().get_line_count() === 2, 'long titles must use two lines');
    assert(info._description.clutter_text.get_layout().get_line_count() === 4, 'long descriptions must use four lines');
    assert(info._credit.clutter_text.get_layout().get_line_count() === 3, 'long credits must use three lines');
    assert(info._credit.visible, 'long descriptions must not displace credit');
    await capture('lock-screen-info-long');

    const theme = St.ThemeContext.get_for_stage(global.stage);
    const originalScale = theme.scale_factor;
    theme.scale_factor = 2;
    await delay();
    if (info._panel.visible) {
        const [px, py] = info._panel.get_position();
        assert(px >= 0 && py >= 0 && px + info._panel.width <= info._holder.width &&
            py + info._panel.height <= info._holder.height, 'scaled information must stay on screen');
        assert(info._credit.y + info._credit.height < info._panel.height,
            'scaled credit must stay inside the panel');
    }
    await capture('lock-screen-info-scaled');
    theme.scale_factor = originalScale;

    writeInfo(data);
    settings.set_boolean('show-wallpaper-info', false);
    await delay();
    settings.set_boolean('show-wallpaper-info', true);
    await until(() => info._panel.visible, 'valid metadata must recover');
    dialog.hide();
    await delay();
    assert(!info._panel.visible, 'unmapped dialogs must hide information');
    dialog.show();
    await until(() => info._panel.visible, 'remapped dialogs must reload information');
    dialog.destroy();
    parent.destroy();
    assert(info._destroyed && info._signals.length === 0 && info._layoutId === 0, 'destroy must release resources');
    print('Spotlit lock screen information checks passed');
}
