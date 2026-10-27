const { test } = require('node:test');
const assert = require('node:assert/strict');
const { parseTsv, toTsv, pasteCells } = require('../assets/grid/logic.js');
const bounds = (top, left, bottom = top, right = left) => ({top, left, bottom, right});
const paste = (source, target, rows = 8, cols = 8) => [...pasteCells(parseTsv(source), target, rows, cols)];

test('TSV: empty cell, CRLF, trailing newline and empty last column', () => {
    assert.deepEqual(parseTsv(''), [['']]);
    assert.deepEqual(parseTsv('a\t\r\nb\t\r\n'), [['a', ''], ['b', '']]);
    assert.deepEqual(parseTsv('a\n\n'), [['a'], ['']]);
});
test('quoted tabs/newlines/quotes and Unicode roundtrip', () => {
    const rows = [['Привет\tмир', 'a\nb', '"quoted"'], ['<script>', '', 'z']];
    assert.deepEqual(parseTsv(toTsv(rows)), rows);
});
test('NULL copies as empty text, string NULL stays literal', () => {
    assert.equal(toTsv([[null, 'NULL']]), '\tNULL');
});
test('single-cell paste replaces its destination', () => {
    assert.deepEqual(paste('new', bounds(2,3)), [{row:2,column:3,value:'new'}]);
});
test('one cell fills selected rectangle including row and column', () => {
    assert.equal(paste('x', bounds(1,1,3,4)).length, 12);
    assert.ok(paste('x', bounds(1,1,3,4)).every(cell => cell.value === 'x'));
});
test('single anchor expands to source dimensions', () => {
    assert.deepEqual(paste('a\tb\nc\td', bounds(1,2)), [
        {row:1,column:2,value:'a'}, {row:1,column:3,value:'b'},
        {row:2,column:2,value:'c'}, {row:2,column:3,value:'d'},
    ]);
});
test('equal rectangles map values exactly', () => {
    assert.deepEqual(paste('a\tb\nc\td', bounds(0,0,1,1)).map(c=>c.value), ['a','b','c','d']);
});
test('larger target tiles both axes including partial repeats', () => {
    assert.deepEqual(paste('a\tb\nc\td', bounds(0,0,2,2)).map(c=>c.value), ['a','b','a','c','d','c','a','b','a']);
});
test('smaller target clips oversized source', () => {
    assert.deepEqual(paste('a\tb\tc\nd\te\tf', bounds(0,0,0,1)).map(c=>c.value), ['a','b']);
});
test('expanded paste clips to loaded rows/columns', () => {
    assert.deepEqual(paste('a\tb\nc\td', bounds(7,7)), [{row:7,column:7,value:'a'}]);
});
test('ragged TSV fills missing fields with empty strings', () => {
    assert.deepEqual(paste('a\tb\nc', bounds(0,0)).map(c=>c.value), ['a','b','c','']);
});
test('no selection and empty dataset cannot modify data', () => {
    assert.deepEqual(paste('x', null), []);
    assert.deepEqual(paste('x', bounds(0,0), 0, 0), []);
});
