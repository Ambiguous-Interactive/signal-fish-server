@0xb68d224da0bc9bda;
struct Input { tick @0 :UInt32; buttons @1 :UInt32; horizontal @2 :Int32; vertical @3 :Int32; }
struct Entity { id @0 :UInt32; position @1 :List(Float64); velocity @2 :List(Float64); health @3 :UInt32; ammo @4 :UInt32; active @5 :Bool; }
struct Snapshot { tick @0 :UInt32; entities @1 :List(Entity); map @2 :Text; }
struct Update { id @0 :UInt32; position @1 :List(Float64); health @2 :UInt32; animation @3 :UInt32; }
struct Tile { x @0 :UInt32; y @1 :UInt32; tileType @2 :UInt32; }
struct Delta { tick @0 :UInt32; baseTick @1 :UInt32; spawned @2 :List(Entity); updated @3 :List(Update); removed @4 :List(UInt32); tiles @5 :List(Tile); }
