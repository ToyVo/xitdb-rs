// Verifies tests/fixtures/rust-golden.db (written by the Rust
// example) contains the expected manifest. Exit non-zero on any mismatch.
const std = @import("std");
const xitdb = @import("src/lib.zig");

const HashInt = u160;
const MAX_READ_BYTES = 4096;

fn hashInt(buffer: []const u8) HashInt {
    var hash: [@bitSizeOf(HashInt) / 8]u8 = undefined;
    std.crypto.hash.Sha1.hash(buffer, &hash, .{});
    return std.mem.readInt(HashInt, &hash, .big);
}

var failures: usize = 0;

fn expect(cond: bool, msg: []const u8) void {
    if (!cond) {
        std.debug.print("FAIL: {s}\n", .{msg});
        failures += 1;
    }
}

pub fn main(init: std.process.Init) !void {
    const allocator = init.gpa;
    const io = init.io;

    var arg_it = try std.process.Args.Iterator.initAllocator(init.minimal.args, allocator);
    defer arg_it.deinit();
    _ = arg_it.next(); // argv[0]
    const db_path = arg_it.next() orelse {
        std.debug.print("usage: verify-rust-golden <rust-golden.db>\n", .{});
        std.process.exit(2);
    };

    const file = try std.Io.Dir.cwd().openFile(io, db_path, .{ .mode = .read_only });
    defer file.close(io);

    const DB = xitdb.Database(.file, HashInt);
    var db = try DB.init(io, .{ .file = file });

    const history = try DB.ArrayList(.read_write).init(db.rootCursor());
    expect(try history.count() == 1, "history count != 1");

    const mc = (try history.getCursor(0)).?;
    const m = try DB.HashMap(.read_only).init(mc);

    const name_cursor = (try m.getCursor(hashInt("name"))).?;
    const name = try name_cursor.readBytesAlloc(allocator, MAX_READ_BYTES);
    defer allocator.free(name);
    expect(std.mem.eql(u8, name, "rust"), "name != rust");

    const tagged_cursor = (try m.getCursor(hashInt("tagged"))).?;
    const tagged = try tagged_cursor.readBytesAlloc(allocator, MAX_READ_BYTES);
    defer allocator.free(tagged);
    expect(std.mem.eql(u8, tagged, "abc"), "tagged != abc");

    const answer_cursor = (try m.getCursor(hashInt("answer"))).?;
    expect(try answer_cursor.readUint() == 42, "answer != 42");

    const list_cursor = (try m.getCursor(hashInt("list"))).?;
    const list = try DB.ArrayList(.read_only).init(list_cursor);
    expect(try list.count() == 2, "list count != 2");

    const sorted_cursor = (try m.getCursor(hashInt("sorted"))).?;
    const sm = try DB.SortedMap(.read_only).init(sorted_cursor);
    expect(try sm.count() == 2, "sorted count != 2");

    if (failures > 0) std.process.exit(1);
    std.debug.print("rust-golden.db OK\n", .{});
}
