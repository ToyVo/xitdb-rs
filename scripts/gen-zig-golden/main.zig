// Generates tests/fixtures/zig-golden.db with a fixed file id.
// Covers every collection kind plus scalars, format tags, and nesting,
// matching the manifest asserted by tests/compat.rs. This is the Zig
// reference output the Rust port checks itself against.
const std = @import("std");
const xitdb = @import("src/lib.zig");

const HashInt = u160;

fn hashInt(buffer: []const u8) HashInt {
    var hash: [@bitSizeOf(HashInt) / 8]u8 = undefined;
    std.crypto.hash.Sha1.hash(buffer, &hash, .{});
    return std.mem.readInt(HashInt, &hash, .big);
}

pub fn main(init: std.process.Init) !void {
    const allocator = init.gpa;
    const io = init.io;

    var arg_it = try std.process.Args.Iterator.initAllocator(init.minimal.args, allocator);
    defer arg_it.deinit();
    _ = arg_it.next(); // argv[0]
    const out_path = arg_it.next() orelse {
        std.debug.print("usage: gen-zig-golden <out.db>\n", .{});
        std.process.exit(2);
    };

    const file = try std.Io.Dir.cwd().createFile(io, out_path, .{ .read = true, .truncate = true });
    defer file.close(io);

    const DB = xitdb.Database(.file, HashInt);
    var db = try DB.init(io, .{
        .file = file,
        .file_id = 1234,
        .hash_id = .fromBytes("sha1"),
    });

    const history = try DB.ArrayList(.read_write).init(db.rootCursor());

    // Moment 0: a small marker.
    try history.appendContext(.{ .slot = try history.getSlot(-1) }, struct {
        pub fn run(_: @This(), cursor: *DB.Cursor(.read_write)) !void {
            const m = try DB.HashMap(.read_write).init(cursor.*);
            try m.put(hashInt("gen"), .{ .uint = 0 });
        }
    }{});

    // Moment 1: the manifest.
    try history.appendContext(.{ .slot = try history.getSlot(-1) }, struct {
        pub fn run(_: @This(), cursor: *DB.Cursor(.read_write)) !void {
            const m = try DB.HashMap(.read_write).init(cursor.*);
            try m.put(hashInt("uint"), .{ .uint = 42 });
            try m.put(hashInt("int"), .{ .int = -7 });
            try m.put(hashInt("float"), .{ .float = 2.5 });
            try m.put(hashInt("tagged"), .{ .bytes_object = .{
                .value = "abc",
                .format_tag = "bi".*,
            } });

            var long: [5000]u8 = undefined;
            for (&long, 0..) |*b, i| b.* = @truncate(i);
            try m.put(hashInt("long"), .{ .bytes = &long });

            const sorted_cursor = try m.putCursor(hashInt("sorted"));
            const sm = try DB.SortedMap(.read_write).init(sorted_cursor);
            for ([_][]const u8{ "c", "a", "b" }) |k| {
                try sm.put(k, .{ .uint = k.len });
            }

            const linked_cursor = try m.putCursor(hashInt("linked"));
            const ll = try DB.LinkedArrayList(.read_write).init(linked_cursor);
            for ([_][]const u8{ "x", "y", "z" }) |s| {
                try ll.append(.{ .bytes = s });
            }

            const hset_cursor = try m.putCursor(hashInt("hset"));
            const hs = try DB.HashSet(.read_write).init(hset_cursor);
            for ([_][]const u8{ "s1", "s2" }) |s| {
                try hs.put(hashInt(s), .{ .bytes = s });
            }

            const chm_cursor = try m.putCursor(hashInt("chm"));
            const chm = try DB.CountedHashMap(.read_write).init(chm_cursor);
            try chm.put(hashInt("k1"), .{ .uint = 1 });
            try chm.put(hashInt("k2"), .{ .uint = 2 });
        }
    }{});

    std.debug.print("wrote {s}\n", .{out_path});
}
