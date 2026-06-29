import {
  Entity, PrimaryGeneratedColumn, Column, ManyToOne, OneToMany, Index,
  CreateDateColumn, UpdateDateColumn,
} from "typeorm";
import { Org } from "./Org";
import { Post } from "./Post";

@Entity({ name: "app_user" })
export class User {
  @PrimaryGeneratedColumn()
  id!: number;

  @Index({ unique: true })
  @Column({ type: "varchar", length: 160 })
  email!: string;

  @Column({ type: "text" })
  name!: string;

  // A JSONB column round-trips structured data.
  @Column({ type: "jsonb", default: {} })
  settings!: Record<string, unknown>;

  @ManyToOne(() => Org, (org) => org.users, { nullable: true, onDelete: "SET NULL" })
  org!: Org | null;

  @OneToMany(() => Post, (post) => post.author)
  posts!: Post[];

  @CreateDateColumn({ type: "timestamptz" })
  createdAt!: Date;

  @UpdateDateColumn({ type: "timestamptz" })
  updatedAt!: Date;
}
