use guardian_db::stores::kv_store::keyvalue::KeyValueIndex;
use guardian_db::iface::StoreIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 Testando KeyValueIndex após refatoração...");
    
    // Cria um novo índice KeyValue
    let mut index = KeyValueIndex::new();
    
    // Testa métodos básicos da trait StoreIndex
    println!("✅ Índice criado com sucesso");
    
    // Testa se está vazio (usando métodos diretos da struct)
    assert!(index.is_empty());
    println!("✅ is_empty() funcionando");
    
    // Testa length (usando métodos diretos da struct)
    assert_eq!(index.len(), 0);
    println!("✅ len() funcionando");
    
    // Testa contains_key (trait StoreIndex - retorna Result)
    assert!(!index.contains_key("test_key")?);
    println!("✅ contains_key() funcionando");
    
    // Testa get_bytes (trait StoreIndex - retorna Result)
    assert!(index.get_bytes("test_key")?.is_none());
    println!("✅ get_bytes() funcionando");
    
    // Testa keys (trait StoreIndex - retorna Result)
    assert!(index.keys()?.is_empty());
    println!("✅ keys() funcionando");
    
    // Testa clear (trait StoreIndex - retorna Result)
    index.clear()?;
    println!("✅ clear() funcionando");
    
    // Testa novamente métodos da trait StoreIndex que retornam Result
    println!("🔍 Testando métodos da trait StoreIndex que retornam Result:");
    assert!(StoreIndex::is_empty(&index)?);
    println!("✅ StoreIndex::is_empty() funcionando");
    
    assert_eq!(StoreIndex::len(&index)?, 0);
    println!("✅ StoreIndex::len() funcionando");
    
    println!("🎉 Todos os testes básicos passaram!");
    println!("🔧 KeyValueIndex está funcionando corretamente após refatoração");
    
    Ok(())
}
