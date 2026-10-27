/* Pure clipboard rules, shared by the embedded grid and Node tests. */
(function (root) {
    "use strict";
    function parseTsv(text) {
        const rows = [], row = [];
        let value = "", quoted = false;
        for (let i = 0; i < text.length; i++) {
            const ch = text[i];
            if (ch === '"' && (quoted || value === "")) {
                if (quoted && text[i + 1] === '"') { value += '"'; i++; }
                else quoted = !quoted;
            } else if (!quoted && ch === "\t") {
                row.push(value); value = "";
            } else if (!quoted && (ch === "\n" || ch === "\r")) {
                if (ch === "\r" && text[i + 1] === "\n") i++;
                row.push(value); rows.push(row.splice(0)); value = "";
                if (i === text.length - 1) return rows;
            } else value += ch;
        }
        row.push(value); rows.push(row);
        return rows;
    }
    function toTsv(rows) {
        return rows.map(row => row.map(value => {
            const text = value === null ? "" : String(value);
            return /[\t\r\n"]/.test(text) ? '"' + text.replace(/"/g, '""') + '"' : text;
        }).join("\t")).join("\n");
    }
    function* pasteCells(source, bounds, rowCount, columnCount) {
        if (!source.length || !source[0].length || !bounds) return;
        const height = source.length, width = source.reduce((max, row) => Math.max(max, row.length), 0);
        const single = bounds.top === bounds.bottom && bounds.left === bounds.right;
        const bottom = Math.min(rowCount - 1, single ? bounds.top + height - 1 : bounds.bottom);
        const right = Math.min(columnCount - 1, single ? bounds.left + width - 1 : bounds.right);
        for (let row = bounds.top; row <= bottom; row++) {
            for (let column = bounds.left; column <= right; column++) {
                yield { row, column, value: source[(row - bounds.top) % height][(column - bounds.left) % width] ?? "" };
            }
        }
    }
    const api = { parseTsv, toTsv, pasteCells };
    if (typeof module === "object" && module.exports) module.exports = api;
    else root.DBDeckClipboard = api;
})(globalThis);
