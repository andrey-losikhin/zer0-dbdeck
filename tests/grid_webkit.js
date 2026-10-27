(async () => {
    const passed = [], errors = [];
    window.addEventListener('error', event => errors.push(event.message+' at '+event.filename+':'+event.lineno+':'+event.colno+' '+event.error?.stack));
    const nativeTimeout = window.setTimeout;
    window.setTimeout = (fn, ms, ...args) => nativeTimeout(() => { try { fn(...args); } catch (error) { errors.push(String(error)+' '+error.stack); } }, ms);
    window.addEventListener('unhandledrejection', event => errors.push(String(event.reason)));
    const wait = (ms = 35) => new Promise(resolve => setTimeout(resolve, ms));
    const assert = (value, message) => { if (!value) throw new Error(message); };
    const test = async (name, run) => { await run(); assert(!errors.length, errors.join('; ')); passed.push(name); };
    const cell = (row, col) => table.getRow(row).getCell(`c${col}`);
    const mouse = (element, type, extra = {}) => element.dispatchEvent(new MouseEvent(type, {bubbles:true, cancelable:true, button:0, buttons:type==='mouseup'?0:1, ...extra}));
    const click = async (element, extra = {}) => {
        mouse(element, 'mousedown', extra); mouse(element, 'mouseup', extra); mouse(element, 'click', extra); await wait();
    };
    const key = (name, extra = {}, target = document.querySelector('.tabulator-tableholder')) =>
        target.dispatchEvent(new KeyboardEvent('keydown', {key:name, code:name, bubbles:true, cancelable:true, ...extra}));
    const range = () => table.getRanges()[0];
    const drag = async (from, to) => {
        mouse(from, 'mousedown'); mouse(to, 'mousemove'); mouse(to, 'mouseup'); await wait();
    };
    const snapshot = async name => {
        window.dbdeckSnapshotDone = false;
        send({type:'testSnapshot', name});
        for (let i=0; i<50 && !window.dbdeckSnapshotDone; i++) await wait(20);
        assert(window.dbdeckSnapshotDone, 'snapshot timeout');
    };
    try {
        await wait(100);
        await test('startup has no range or active border', async () => {
            assert(table.getRanges().length === 0, 'initial range');
            assert(!table.modules.selectRange.activeRange, 'initial activeRange');
            assert(getComputedStyle(table.modules.selectRange.overlay).visibility === 'hidden', 'visible overlay');
        });
        await test('column boundary drag resizes without selecting or sorting', async () => {
            const column = table.getColumn('c1');
            const before = column.getWidth();
            const handle = column.getElement().nextElementSibling;
            assert(handle?.classList.contains('tabulator-col-resize-handle'), 'missing resize handle');
            const x = handle.getBoundingClientRect().x;
            mouse(handle, 'mousedown', {clientX:x, screenX:x});
            mouse(document.body, 'mousemove', {clientX:x+65, screenX:x+65});
            mouse(document.body, 'mouseup', {clientX:x+65, screenX:x+65});
            await wait();
            assert(column.getWidth() >= before+60, 'boundary drag did not resize');
            assert(!table.getRanges().length, 'resize selected a range');
            assert(getComputedStyle(handle).cursor === 'col-resize', 'resize cursor missing');
            const element = cell(1,1).getElement();
            assert(element.title === String(cell(1,1).getValue()), 'full cell text tooltip');
            assert(getComputedStyle(element).textOverflow === 'ellipsis', 'clipped text lacks ellipsis');
            column.setWidth(before);
        });
        await test('single click selects exactly one cell without an editor', async () => {
            await click(cell(1,1).getElement());
            assert(range().getCells().flat().length === 1, 'single range');
            assert(range().getRows()[0].getIndex() === 1, 'wrong row');
            assert(range().getColumns()[0].getField() === 'c1', 'wrong column');
            assert(!document.querySelector('.tabulator-editing input'), 'single click edited');
            assert(getComputedStyle(cell(1,1).getElement()).cursor === 'default', 'cell has text cursor');
            assert(window.getSelection().toString() === '', 'cell click selected DOM text');
            const select = new Event('selectstart', {bubbles:true,cancelable:true});
            cell(1,1).getElement().dispatchEvent(select);
            assert(select.defaultPrevented, 'table allows native text selection');
            await snapshot('cell');
        });
        await test('F2 editor and keyboard cell menu respect Escape', async () => {
            key('F2'); await wait();
            const editor = document.querySelector('.tabulator-editing input');
            assert(editor, 'F2 did not edit selected cell');
            key('Escape', {}, editor); await wait();
            key('Enter'); await wait();
            const enterEditor = document.querySelector('.tabulator-editing input');
            assert(enterEditor, 'Enter did not edit selected cell');
            key('Enter', {}, enterEditor); await wait();
            assert(!editingCell, 'Enter did not finish editing');
            key('F10', {shiftKey:true}); await wait();
            assert(document.querySelector('.tabulator-menu'), 'keyboard context menu missing');
            key('ArrowDown'); await wait();
            assert(document.activeElement.getAttribute('role') === 'menuitem', 'menu keyboard focus missing');
            key('Escape'); await wait();
            assert(!document.querySelector('.tabulator-menu'), 'Escape did not close menu');
        });
        await test('double click opens inline editor and Escape cancels its value', async () => {
            const before = cell(1,1).getValue();
            mouse(cell(1,1).getElement(), 'dblclick', {detail:2}); await wait();
            const input = document.querySelector('.tabulator-editing input');
            assert(input, 'no inline editor');
            const style = getComputedStyle(input);
            const themeColor = variable => {
                const probe = document.createElement('span');
                probe.style.color = `var(${variable})`;
                document.body.append(probe);
                const color = getComputedStyle(probe).color;
                probe.remove();
                return color;
            };
            assert(style.cursor === 'text' && style.color === themeColor('--grid-fg'), 'editor cursor or theme text color');
            assert(style.backgroundColor === themeColor('--grid-panel'), 'editor theme background');
            assert(style.color !== style.backgroundColor, 'editor text is invisible');
            input.value = 'cancel me'; input.setSelectionRange(0, 6);
            const select = new Event('selectstart', {bubbles:true,cancelable:true});
            input.dispatchEvent(select);
            assert(!select.defaultPrevented && input.selectionEnd === 6, 'editor blocks text selection');
            await snapshot('editor');
            key('Escape', {}, input); await wait();
            assert(!editingCell && cell(1,1).getValue() === before, 'Escape did not cancel');
        });
        await test('drag selects rectangular cells with one outer border', async () => {
            await drag(cell(1,1).getElement(), cell(3,3).getElement());
            assert(range().getRows().length === 3 && range().getColumns().length === 3, 'drag bounds');
            assert(document.querySelectorAll('.tabulator-range').length === 1, 'multiple borders');
            assert(window.getSelection().toString() === '', 'range drag selected DOM text');
            await snapshot('range');
        });
        await test('Escape clears every range, Shift restarts and extends from new anchor', async () => {
            key('Escape'); await wait();
            assert(!table.getRanges().length && !table.modules.selectRange.activeRange, 'Escape retained selection');
            key('ArrowDown');
            assert(range().getCells().flat().length === 1, 'keyboard did not initialize selection');
            key('Escape');
            await click(cell(2,2).getElement(), {shiftKey:true});
            await click(cell(4,3).getElement(), {shiftKey:true});
            assert(range().getRows().length === 3 && range().getColumns().length === 2, 'Shift after Escape');
        });
        await test('row-header drag keeps all adjacent rows and columns', async () => {
            await drag(table.getRow(1).getCells()[0].getElement(), table.getRow(3).getCells()[0].getElement());
            assert(range().getRows().length === 3 && range().getColumns().length === 12, 'row drag');
            await snapshot('rows');
        });
        await test('column-header drag keeps adjacent columns and all loaded rows', async () => {
            await drag(table.getColumn('c1').getElement(), table.getColumn('c3').getElement());
            assert(range().getRows().length === 240 && range().getColumns().length === 3, 'column drag');
            await snapshot('columns');
        });
        await test('sort only on icon, server sort never reorders staged row identities', async () => {
            const before = table.getRows('active').map(row=>row.getIndex()).join(',');
            await click(table.getColumn('c1').getElement());
            assert(table.getSorters().length === 0, 'header sorted');
            await click(table.getColumn('c1').getElement().querySelector('.tabulator-col-sorter'));
            assert(table.getRows('active').map(row=>row.getIndex()).join(',') === before, 'server sort reordered locally');
            window.dbdeckSetSort({field:'c1',dir:'asc'});
            assert(table.getColumn('c1').getElement().getAttribute('aria-sort') === 'ascending', 'ascending indicator');
            assert(getComputedStyle(table.getColumn('c1').getElement().querySelector('.tabulator-arrow')).borderBottomWidth === '6px', 'ascending arrow');
            window.dbdeckSetSort({field:'c1',dir:'desc'});
            assert(table.getColumn('c1').getElement().getAttribute('aria-sort') === 'descending', 'descending indicator');
            assert(getComputedStyle(table.getColumn('c1').getElement().querySelector('.tabulator-arrow')).borderTopWidth === '6px', 'descending arrow');
            window.dbdeckSetSort(null);

        });
        await test('Ctrl+A selects only loaded data including virtual rows', async () => {
            key('a', {ctrlKey:true}); await wait();
            assert(range().getRows().length === 240 && range().getColumns().length === 12, 'select all: '+range().getRows().length+'x'+range().getColumns().length+' fields '+table.getColumns().map(c=>c.getField()));
        });
        await test('native GDK clipboard roundtrip replaces chosen single cell', async () => {
            await click(cell(1,1).getElement()); window.dbdeckCopy(); await wait();
            assert(copiedRange && getComputedStyle(copiedRange).borderStyle === 'dashed', 'copy border');
            await snapshot('copied');
            const value = cell(1,1).getValue();
            await click(cell(3,3).getElement());
            assert(copiedRange, 'copy border lost on target selection');
            window.dbdeckRequestPaste(); await wait(200);
            assert(cell(3,3).getValue() === value, 'clipboard did not paste into target');
        });
        await test('range clipboard shortcuts support Russian layout and Insert combinations', async () => {
            await click(cell(1,1).getElement());
            const value = cell(1,1).getValue();
            key('с', {ctrlKey:true, code:'KeyC'}); await wait(80);
            assert(copiedRange && getComputedStyle(copiedRange).borderStyle === 'dashed', 'Ctrl+C lacks dashed border');
            await drag(cell(5,5).getElement(), cell(7,7).getElement());
            key('м', {ctrlKey:true, code:'KeyV'}); await wait(150);
            assert(range().getCells().flat().every(cell => cell.getValue() === value), 'Ctrl+V did not fill range');
            key('Insert', {ctrlKey:true}); await wait(80);
            assert(copiedRange, 'Ctrl+Insert failed');
            await drag(cell(8,5).getElement(), cell(10,7).getElement());
            key('Insert', {shiftKey:true}); await wait(150);
            assert(range().getCells().flat().every(cell => cell.getValue() === value), 'Shift+Insert did not fill range');
            await drag(cell(11,5).getElement(), cell(13,7).getElement());
            key('V', {ctrlKey:true,shiftKey:true,code:'KeyV'}); await wait(150);
            assert(range().getCells().flat().every(cell => cell.getValue() === value), 'Ctrl+Shift+V did not fill range');
            assert(window.getSelection().toString() === '', 'clipboard selected DOM text');
        });
        await test('delayed clipboard result cannot modify a different selection', async () => {
            const token = clipboardToken;
            await click(cell(5,5).getElement());
            const before = cell(5,5).getValue();
            window.dbdeckPaste('stale clipboard', token);
            assert(cell(5,5).getValue() === before, 'stale paste wrote wrong target');
        });
        await test('native Save finishes editor before action; Discard cancels editor', async () => {
            await click(cell(1,9).getElement());
            mouse(cell(1,9).getElement(), 'dblclick', {detail:2}); await wait();
            document.querySelector('.tabulator-editing input').value = 'native-save';
            window.dbdeckLastAction = null;
            send({type:'testFinishEdit', action:'save'}); await wait(100);
            assert(window.dbdeckLastAction === 'save' && !editingCell && cell(1,9).getValue() === 'native-save', 'save ordering');
            mouse(cell(1,9).getElement(), 'dblclick', {detail:2}); await wait();
            document.querySelector('.tabulator-editing input').value = 'cancel-native';
            send({type:'testFinishEdit', action:'discard'}); await wait(100);
            assert(window.dbdeckLastAction === 'discard' && !editingCell && cell(1,9).getValue() === 'native-save', 'discard editor');
        });
        await test('saving lock blocks programmatic toolbar paste', async () => {
            await click(cell(1,1).getElement());
            const before = cell(1,1).getValue();
            window.dbdeckSetLocked(true); window.dbdeckPaste('during-save');
            assert(cell(1,1).getValue() === before, 'locked paste changed data');
            window.dbdeckSetLocked(false);
        });
        await test('matrix TSV paste obeys editor permissions and sends staged edits', async () => {
            await click(cell(1,1).getElement()); window.dbdeckPaste('a\tb\nc\td'); await wait();
            assert(cell(1,1).getValue() === 'a' && cell(1,2).getValue() === 'b', 'matrix first row');
            assert(cell(2,1).getValue() === 'c' && cell(2,2).getValue() === '2:2', 'blocked cell overwritten');
            await click(cell(1,0).getElement()); window.dbdeckPaste('overwrite PK');
            assert(cell(1,0).getValue() === '1:0', 'PK overwritten');
        });
        await test('rectangle tiling, clipping and row/column paste', async () => {
            await drag(cell(1,1).getElement(), cell(3,3).getElement()); window.dbdeckPaste('a\tb\nc\td');
            assert(cell(3,3).getValue() === 'a' && cell(3,2).getValue() === 'b', 'tiling');
            await drag(cell(1,1).getElement(), cell(1,2).getElement()); window.dbdeckPaste('1\t2\t3\n4\t5\t6');
            assert(cell(1,1).getValue() === '1' && cell(1,2).getValue() === '2', 'clip');
            await click(table.getRow(4).getCells()[0].getElement()); window.dbdeckPaste('row');
            assert(cell(4,11).getValue() === 'row' && cell(4,0).getValue() === '4:0', 'row paste');
            await click(table.getColumn('c4').getElement()); window.dbdeckPaste('col');
            assert(cell(239,4).getValue() === 'col', 'column paste misses virtual rows');
        });
        await test('copy row, column and rectangle includes all virtual values', async () => {
            for (const [start, end, rowCount, colCount] of [[cell(1,1),cell(3,3),3,3], [cell(1,0),cell(1,11),1,12], [cell(0,1),cell(239,1),240,1]]) {
                rangeAdapter.clear(); table.addRange(start, end); await wait(); window.dbdeckCopy();
                const values = range().getCells();
                assert(values.length === rowCount && values[0].length === colCount, 'copy dimensions');
            }
        });
        await test('virtual scroll and append preserve selection and staged values', async () => {
            await table.scrollToRow(235, 'center', false); await wait(100);
            assert(document.querySelectorAll('.tabulator-row').length < 240, 'not virtualized');
            const before = cell(239,4).getValue();
            send({type:'testAppend'}); await wait(200);
            assert(table.getDataCount() === 260, 'append count');
            assert(cell(239,4).getValue() === before, 'append lost edits');
            assert(range().getRows().length === 240, 'append lost selection');
            key('Escape'); await wait(); assert(!table.getRanges().length, 'Escape after scroll');
        });
        await test('replacement/discard clears edits and selection; SQL remains read-only and locally sortable', async () => {
            const payload = {server_sort:false, columns:[{title:'result',field:'c0',editable:false}], rows:[
                {index:0,values:['z'],editable:[true]}, {index:1,values:['a'],editable:[true]}]};
            await window.dbdeckSetData(payload, false); await wait(80);
            assert(!table.getRanges().length, 'replacement selection');
            assert(!document.querySelector('.dbdeck-edited'), 'replacement staged style');
            await click(cell(0,0).getElement()); mouse(cell(0,0).getElement(), 'dblclick', {detail:2});
            window.dbdeckPaste('denied'); await wait(); assert(cell(0,0).getValue() === 'z' && !editingCell, 'SQL editable');
            await click(table.getColumn('c0').getElement().querySelector('.tabulator-col-sorter'));
            assert(table.getRows('active')[0].getData().c0 === 'a', 'SQL local sorting');
            key('Escape'); await click(table.getColumn('c0').getElement(), {shiftKey:true});
            assert(range().getRows().length === 2, 'Shift column after Escape');
            key('Escape'); await click(table.getRow(0).getCells()[0].getElement(), {shiftKey:true});
            assert(range().getColumns().length === 1, 'Shift row after Escape');
        });
        send({type:'testResult', passed, errors});
    } catch (error) { send({type:'testResult', passed, errors:[...errors, String(error) + "\n" + error.stack]}); }
})();
