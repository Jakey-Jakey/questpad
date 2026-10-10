import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.data.PointerDataType;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;

public class TypePools extends GhidraScript {
    @Override
    public void run() throws Exception {
        Memory mem = currentProgram.getMemory();
        Listing lst = currentProgram.getListing();
        int n = 0;
        for (Instruction ins : lst.getInstructions(true)) {
            for (Reference r : ins.getReferencesFrom()) {
                if (!r.getReferenceType().isRead()) continue;
                Address a = r.getToAddress();
                MemoryBlock b = mem.getBlock(a);
                if (b == null || !b.isInitialized() || b.isWrite() || (a.getOffset() & 3) != 0) continue;
                long v = mem.getInt(a) & 0xffffffffL;
                Address va = toAddr(v);
                if (mem.getBlock(va) == null || v < 0x100) continue;
                Data dd = lst.getDataAt(a);
                if (dd != null && dd.isPointer()) continue;
                if (lst.getInstructionContaining(a) != null) continue;
                try { lst.clearCodeUnits(a, a.add(3), false); lst.createData(a, new PointerDataType()); n++; } catch (Exception e) {}
            }
        }
        println("typed pointers: " + n);
    }
}
