"use strict";
const send = message => window.webkit.messageHandlers.dbdeck.postMessage(message);
let table = null, columnSignature = null;
let hasMore = false, loadingMore = false, updatingData = false;
let editingCell = null, editingOriginal = "", copiedRange = null, rangeAdapter = null;
let rowsWritable = false;
let serverSort = false, serverSorter = null, clipboardToken = 0, locked = false;
window.dbdeckSetLocked = value => { locked = value; clipboardToken++; };

// Tabulator menus are divs; give their actions native keyboard focus semantics.
new MutationObserver(() => {
    document.querySelectorAll('.tabulator-menu .tabulator-menu-item:not(.tabulator-menu-item-disabled)').forEach(item => {
        if (item.hasAttribute('tabindex')) return;
        item.tabIndex = 0;
        item.setAttribute('role', 'menuitem');
    });
}).observe(document.body, {childList:true, subtree:true});

function renderServerSort() {
    if (!table || !serverSort) return;
    table.getColumns().forEach(column => {
        const direction = serverSorter && column.getField() === serverSorter.field ? serverSorter.dir : null;
        column.getElement().setAttribute("aria-sort", direction === "asc" ? "ascending" : direction === "desc" ? "descending" : "none");
    });
}
window.dbdeckSetSort = sort => { serverSorter = sort; renderServerSort(); };

// Version-specific SelectRange access stays here. Tabulator 6.6.1 leaves a stale
// activeRange after clearRanges; keyboard/Shift/right-click assume it is valid.
function installRangeAdapter(grid) {
    const manager = grid.modules.selectRange;
    const originalClear = manager.clearRanges.bind(manager);
    manager.clearRanges = () => {
        manager.activeRange = false;
        originalClear();
        manager.overlay.style.visibility = "hidden";
    };
    // Upstream initializeFocus selects the cell's DOM text for its clipboard.
    // DBDeck uses GDK clipboard and must focus the grid without a text selection.
    manager.initializeFocus = () => {
        if (editingCell) return;
        window.getSelection()?.removeAllRanges();
        manager.restoreFocus();
    };
    return {
        closeMenu() {
            // Tabulator binds outside-click dismissal after 100ms; Escape must work immediately.
            grid.modules.menu.rootPopup?.hide();
            manager.restoreFocus();
        },
        clear() {
            manager.clearRanges();
            manager.mousedown = false;
            manager.selecting = "cell";
            manager.layoutElement(true);
        },
        ensureAnchor(event) {
            if (manager.activeRange || (!event.shiftKey && event.button !== 2)) return;
            const element = event.target.closest(".tabulator-cell,.tabulator-col");
            if (!element || !grid.getDataCount()) return;
            const field = element.getAttribute("tabulator-field") || grid.getColumns().find(column => column.getField())?.getField();
            const rowElement = element.closest(".tabulator-row");
            const row = rowElement ? grid.getRows("visible").find(row => row.getElement() === rowElement) : grid.getRows("active")[0];
            const cell = row?.getCell(field);
            if (cell) grid.addRange(cell, cell);
        },
    };
}
function selectionBounds(range = table?.getRanges()[0]) {
    if (!range) return null;
    return { top: range.getTopEdge(), bottom: range.getBottomEdge(),
        left: Number(range.getColumns()[0]?.getField().slice(1)),
        right: Number(range.getColumns().at(-1)?.getField().slice(1)) };
}
function clearCopy() {
    copiedRange?.remove(); copiedRange = null;
    document.querySelectorAll(".dbdeck-source-copied").forEach(element => element.classList.remove("dbdeck-source-copied"));
}
function clearSelection() {
    clipboardToken++;
    clearCopy();
    rangeAdapter?.clear();
    window.getSelection()?.removeAllRanges();
    send({ type: "clearSelection" });
}
function syncRange(range) {
    if (!range || updatingData) return;
    clipboardToken++;
    range.getElement().classList.remove("dbdeck-source-copied");
    const rows = range.getRows(), columns = range.getColumns();
    if (!rows.length || !columns.length) return;
    // Use public Row/Column Components: Range.getBounds() exposes internal Cells.
    send({ type: "selection", top: rows[0].getData()._row,
        bottom: rows.at(-1).getData()._row, left: columns[0].getField(),
        right: columns.at(-1).getField(),
        fullColumn: rows.length === table.getDataCount() && columns.length === 1 });
}
function canEdit(cell) {
    const definition = cell.getColumn().getDefinition();
    return !locked && definition.editor !== false && definition.editable(cell);
}
function applyValue(cell, value) {
    if (canEdit(cell) && cell.getValue() !== value) cell.setValue(value);
}
function selectedCells() { return table?.getRanges()[0]?.getCells().flat() ?? []; }
window.dbdeckCopy = () => {
    if (editingCell) return;
    const range = table?.getRanges()[0];
    if (!range) return;
    const text = DBDeckClipboard.toTsv(range.getCells().map(row => row.map(cell => cell.getValue())));
    send({ type: "clipboardCopy", text });
    clearCopy();
    copiedRange = range.getElement().cloneNode(false);
    copiedRange.className = "dbdeck-copied-range";
    range.getElement().classList.add("dbdeck-source-copied");
    // A sibling of the range overlay keeps the copied border while selecting a target.
    table.element.querySelector(".tabulator-tableholder").append(copiedRange);
};
window.dbdeckRequestPaste = () => { if (!locked && !editingCell && table?.getRanges().length) send({ type: "clipboardPaste", token: ++clipboardToken }); };
window.dbdeckPaste = (text, token = clipboardToken) => {
    if (!table || locked || editingCell || token !== clipboardToken) return;
    const rows = table.getRows("active"), columns = table.getColumns().filter(column => column.getField());
    const source = DBDeckClipboard.parseTsv(text);
    const bounds = selectionBounds();
    table.blockRedraw();
    try {
        for (const target of DBDeckClipboard.pasteCells(source, bounds, rows.length, columns.length)) {
            const cell = rows[target.row].getCell(`c${target.column}`);
            if (cell) applyValue(cell, target.value);
        }
    } finally { table.restoreRedraw(); }
    clearCopy();
};
window.dbdeckFinishEdit = action => {
    if (locked) return;
    if (editingCell) {
        const cell = editingCell;
        const input = cell.getElement().querySelector("input,textarea");
        const value = input?.value;
        // A native GTK button can remove focus before WebKit delivers blur.
        // Finish explicitly, using the same permission/event path as paste.
        cell.cancelEdit();
        if (action === "save" && input) applyValue(cell, value);
    }
    // The edit message is posted synchronously before this action.
    if (!editingCell) send({ type: "editAction", action });
};
function selectAll() {
    const rows = table.getRows("active"), columns = table.getColumns().filter(column => column.getField());
    if (!rows.length || !columns.length) return;
    rangeAdapter.clear();
    table.addRange(rows[0].getCell(columns[0].getField()), rows.at(-1).getCell(columns.at(-1).getField()));
}
function asData(rows) {
    return rows.map(row => {
        const data = { _row: row.index };
        row.values.forEach((value, i) => { data[`c${i}`] = value; data[`e${i}`] = row.editable[i] ?? true; });
        return data;
    });
}
function maybeLoadMore() {
    if (!hasMore || loadingMore || updatingData) return;
    const last = table.getRows("visible").at(-1)?.getPosition();
    if (last >= table.getDataCount() - 40) {
        loadingMore = true; send({ type: "loadMore" });
    }
}
window.dbdeckSetData = (payload, more) => {
    rowsWritable = !!payload.row_actions;
    if (editingCell) editingCell.cancelEdit();
    hasMore = more; loadingMore = false;
    serverSort = payload.server_sort;
    const measure = document.createElement('canvas').getContext('2d');
    measure.font = getComputedStyle(document.body).font;
    const columns = payload.columns.map((column, index) => ({
        title: column.title, field: column.field, headerSort: true, resizable: true,
        width: Math.ceil(Math.min(360, Math.max(80, measure.measureText(column.title).width + 42,
            ...payload.rows.map(row => measure.measureText(String(row.values[index] ?? 'NULL').split(/\r?\n/)[0]).width + 20)))),
        editor: column.editable ? "input" : false,
        editable: cell => column.editable && cell.getRow().getData()[`e${column.field.slice(1)}`],
        headerTooltip: column.title,
        formatter: cell => {
            const text = cell.getValue() === null ? "NULL" : String(cell.getValue());
            cell.getElement().title = text;
            return document.createTextNode(text);
        },
    }));
    const signature = JSON.stringify(payload.columns);
    const data = asData(payload.rows);
    updatingData = true;
    if (table) {
        clearSelection();
        const updateColumns = columnSignature !== signature;
        columnSignature = signature;
        // Horizontal virtual rendering caches columns on existing Row objects.
        // Retire those rows before replacing the column layout (e.g. a new SQL result).
        const prepare = updateColumns
            ? table.setData([]).then(() => table.setColumns(columns))
            : Promise.resolve();
        return prepare.then(() => {
            if (serverSort) table.clearSort();
            return table.setData(data);
        }).then(() => {
            rangeAdapter.clear(); updatingData = false; renderServerSort(); send({type:"dataReady"}); maybeLoadMore();
        });
    }
    columnSignature = signature;
    table = new Tabulator("#grid", {
        data, columns, layout: "fitDataFill", height: "100%", index: "_row",
        headerSortClickElement: "icon",
        rowHeader: { formatter: "rownum", width: 42, headerSort: false, hozAlign: "center", resizable: false },
        columnDefaults: { resizable: true, headerSort: false },
        selectableRange: 1, selectableRangeInitializeDefault: false,
        selectableRangeAutoFocus: false, selectableRangeColumns: true, selectableRangeRows: true,
        selectableRangeClearCells: false, clipboard: false, editTriggerEvent: "dblclick",
        rowHeight: 28, renderVertical: "virtual", renderHorizontal: "virtual", columnHeaderVertAlign: "middle",
        rowContextMenu: () => [
            { label: "Удалить строку", disabled: locked || !rowsWritable, action: (e, row) => send({ type: "rowAction", row: row.getData()._row, action: "delete" }) },
        ],
        cellContextMenu: [
            { label: "Копировать", action: () => window.dbdeckCopy() },
            { label: "Вставить", action: () => window.dbdeckRequestPaste() },
            { label: "Установить NULL", action: () => selectedCells().forEach(cell => applyValue(cell, null)) },
        ],
    });
    table.on("tableBuilt", () => {
        rangeAdapter = installRangeAdapter(table);
        // Programmatic row navigation can update Tabulator's cached scroll offset
        // before its public scrollVertical event; also observe the native viewport.
        document.querySelector('.tabulator-tableholder').addEventListener('scroll', maybeLoadMore);
        clearSelection(); updatingData = false;
        renderServerSort(); send({ type: "ready" }); send({type:"dataReady"}); maybeLoadMore();
    });
    table.on("cellEditing", cell => {
        editingCell = cell;
        editingOriginal = String(cell.getValue() ?? "");
        send({type:"editing",active:true,dirty:false});
    });
    table.on("cellEditCancelled", () => { editingCell = null; send({type:"editing",active:false}); });
    table.on("cellEdited", cell => {
        editingCell = null;
        send({type:"editing",active:false});
        cell.getElement().classList.toggle("dbdeck-edited", cell.getValue() !== cell.getInitialValue());
        send({ type: "edit", row: cell.getRow().getData()._row, column: cell.getField(), value: cell.getValue() });
    });
    table.on("rangeAdded", syncRange);
    table.on("rangeChanged", syncRange);
    table.on("dataSorted", sorters => {
        if (updatingData) return;
        clearSelection();
        const sorter = sorters[0];
        send(sorter ? { type: "sort", column: sorter.field, direction: sorter.dir } : { type: "sortClear" });
    });
    table.on("renderComplete", maybeLoadMore);
    table.on("scrollVertical", maybeLoadMore);
    document.querySelector("#grid").addEventListener("click", event => {
        if (!serverSort || !event.target.closest(".tabulator-col-sorter")) return;
        event.preventDefault(); event.stopImmediatePropagation();
        const header = event.target.closest(".tabulator-col"), field = header.getAttribute("tabulator-field");
        const direction = serverSorter?.field === field && serverSorter.dir === "asc" ? "desc" : "asc";
        send({ type: "sort", column: field, direction });
    }, true);
    document.querySelector("#grid").addEventListener("mousedown", event => {
        // Let the handle receive its target listener; stopping in capture disables resizing.
        if (event.target.closest(".tabulator-col-resize-handle")) return;
        if (event.target.closest(".tabulator-col-sorter")) {
            event.stopPropagation(); return;
        }
        if (event.target.closest("input,textarea")) return;
        rangeAdapter?.ensureAnchor(event);
    }, true);
    const inEditor = target => Boolean(target?.closest?.(".tabulator-editing input,.tabulator-editing textarea"));
    document.querySelector("#grid").addEventListener("input", event => {
        if (editingCell && inEditor(event.target)) {
            send({type:"editing",active:true,dirty:event.target.value !== editingOriginal});
        }
    }, true);
    document.querySelector("#grid").addEventListener("selectstart", event => {
        if (!inEditor(event.target)) event.preventDefault();
    });
    document.addEventListener("copy", event => {
        if (editingCell || inEditor(event.target) || !table.getRanges().length) return;
        event.preventDefault();
        const rows = table.getRanges()[0].getCells().map(row => row.map(cell => cell.getValue()));
        event.clipboardData?.setData("text/plain", DBDeckClipboard.toTsv(rows));
        window.dbdeckCopy();
    });
    document.addEventListener("paste", event => {
        if (editingCell || inEditor(event.target) || !table.getRanges().length) return;
        event.preventDefault();
        if (event.clipboardData) window.dbdeckPaste(event.clipboardData.getData("text/plain"));
        else window.dbdeckRequestPaste();
    });
    document.addEventListener("keydown", event => {
        const menu = document.querySelector('.tabulator-menu');
        if (menu && ['ArrowDown', 'ArrowUp', 'Enter', ' ', 'Escape'].includes(event.key)) {
            event.preventDefault(); event.stopImmediatePropagation();
            const items = [...menu.querySelectorAll('.tabulator-menu-item:not(.tabulator-menu-item-disabled)')];
            const current = items.indexOf(document.activeElement);
            if (event.key === 'Escape') rangeAdapter.closeMenu();
            else if (event.key === 'Enter' || event.key === ' ') items[current]?.click();
            else items[(current + (event.key === 'ArrowUp' ? items.length - 1 : 1)) % items.length]?.focus();
            return;
        }
        if (editingCell) return; // The inline editor owns Escape and text clipboard shortcuts.
        const ctrl = event.ctrlKey || event.metaKey;
        // Physical keys keep shortcuts working with Russian and other layouts.
        const letter = /^Key[ACV]$/.test(event.code) ? event.code.slice(3).toLowerCase() : event.key.toLowerCase();
        const insert = event.key === "Insert" || event.code === "Insert";
        const copy = (ctrl && letter === "c") || (ctrl && insert);
        const paste = (ctrl && letter === "v") || (event.shiftKey && insert);
        const all = ctrl && letter === "a";
        if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
            event.preventDefault(); event.stopImmediatePropagation();
            const cell = table.getRanges()[0]?.getCells()[0]?.[0]?.getElement();
            if (cell) {
                const rect = cell.getBoundingClientRect();
                cell.dispatchEvent(new MouseEvent('contextmenu', {bubbles:true, cancelable:true, button:2, clientX:rect.left+8, clientY:rect.bottom}));
                const item = document.querySelector('.tabulator-menu-item:not(.tabulator-menu-item-disabled)');
                if (item) { item.tabIndex = 0; item.focus(); }
            }
        } else if (event.key === "Escape") {
            event.preventDefault(); event.stopImmediatePropagation(); clearSelection();
        } else if (copy || paste || all) {
            event.preventDefault(); event.stopImmediatePropagation();
            if (all) selectAll();
            else if (copy) window.dbdeckCopy();
            else window.dbdeckRequestPaste();
        } else if (event.key === "F2") {
            event.preventDefault(); event.stopImmediatePropagation();
            if (!locked) table.getRanges()[0]?.getCells()[0]?.[0]?.edit();
        } else if (!table.getRanges().length && ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Enter", "Tab", "Home", "End"].includes(event.key)) {
            // Start keyboard navigation safely after Escape or first focus.
            event.preventDefault(); event.stopImmediatePropagation();
            const first = table.getRows("active")[0]?.getCell(table.getColumns().find(column => column.getField())?.getField());
            if (first) table.addRange(first, first);
        } else if (["Delete", "Backspace"].includes(event.key)) {
            event.preventDefault(); event.stopImmediatePropagation();
            selectedCells().forEach(cell => applyValue(cell, null));
        }
    }, true);
};
window.dbdeckAppendData = (rows, more) => {
    const range = table.getRanges()[0];
    const selectedRows = range?.getRows(), selectedColumns = range?.getColumns();
    const start = selectedRows?.[0]?.getCell(selectedColumns[0]?.getField());
    const end = selectedRows?.at(-1)?.getCell(selectedColumns.at(-1)?.getField());
    hasMore = more;
    updatingData = true;
    return table.addData(asData(rows), false).then(() => {
        rangeAdapter.clear(); updatingData = false; loadingMore = false;
        if (start && end) table.addRange(start, end);
        maybeLoadMore();
    });
};
