
const assert = require('assert')
const util = require('util')
const Automerge = require('..')

describe('Automerge', () => {
    describe('basics', () => {
        it('should init clone and free', () => {
            let doc1 = Automerge.init()
            let doc2 = Automerge.clone(doc1);
        })

        it('handle basic set and read on root object', () => {
            let doc1 = Automerge.init()
            let doc2 = Automerge.change(doc1, (d) => {
              d.hello = "world"
              d.big = "little"
              d.zip = "zop"
              d.app = "dap"
            assert.deepEqual(d, {  hello: "world", big: "little", zip: "zop", app: "dap" })
            })
            assert.deepEqual(doc2, {  hello: "world", big: "little", zip: "zop", app: "dap" })
        })

        it('handle basic sets over many changes', () => {
            let doc1 = Automerge.init()
            let timestamp = new Date();
            let counter = new Automerge.Counter(100);
            let bytes = new Uint8Array([10,11,12]);
            let doc2 = Automerge.change(doc1, (d) => {
              d.hello = "world"
            })
            let doc3 = Automerge.change(doc2, (d) => {
              d.counter1 = counter
            })
            let doc4 = Automerge.change(doc3, (d) => {
              d.timestamp1 = timestamp
            })
            let doc5 = Automerge.change(doc4, (d) => {
              d.app = null
            })
            let doc6 = Automerge.change(doc5, (d) => {
              d.bytes1 = bytes
            })
            let doc7 = Automerge.change(doc6, (d) => {
              d.uint = new Automerge.Uint(1)
              d.int = new Automerge.Int(-1)
              d.float64 = new Automerge.Float64(5.5)
              d.number1 = 100
              d.number2 = -45.67
              d.true = true
              d.false = false
            })
            Automerge.dump(doc7)
            //assert.deepEqual(Automerge.toJS(doc6), {  hello: "world", true: true, false: false, int: -1, uint: 1, float64: 5.5, number1: 100, number2: -45.67, counter1: counter, timestamp1: timestamp, bytes1: bytes, app: null })
            assert.deepEqual(doc6, {  hello: "world", true: true, false: false, int: -1, uint: 1, float64: 5.5, number1: 100, number2: -45.67, counter1: counter, timestamp1: timestamp, bytes1: bytes, app: null })
        })

        it('handle overwrites to values', () => {
            let doc1 = Automerge.init()
            let doc2 = Automerge.change(doc1, (d) => {
              d.hello = "world1"
            })
            let doc3 = Automerge.change(doc2, (d) => {
              d.hello = "world2"
            })
            let doc4 = Automerge.change(doc3, (d) => {
              d.hello = "world3"
            })
            let doc5 = Automerge.change(doc4, (d) => {
              d.hello = "world4"
            })
            assert.deepEqual(doc5, {  hello: "world4" } )
            Automerge.dump(doc5)
        })

        it('handle set with object value', () => {
            let doc1 = Automerge.init()
            let doc2 = Automerge.change(doc1, (d) => {
              d.subobj = { hello: "world", subsubobj: { zip: "zop" } }
            })
            //assert.deepEqual(Automerge.toJS(doc2), { subobj:  { hello: "world", subsubobj: { zip: "zop" } } })
            assert.deepEqual(doc2, { subobj:  { hello: "world", subsubobj: { zip: "zop" } } })
        })

      /*
        it('handle set with object value', () => {
            let doc1 = Automerge.init()
            let doc2 = Automerge.change(doc1, (d) => { d.hello = "world" })
            assert.deepEqual(Automerge.toJS(doc2), { subobj:  { hello: "world", subsubobj: { zip: "zop" } } })
        })
        */
    })
})
