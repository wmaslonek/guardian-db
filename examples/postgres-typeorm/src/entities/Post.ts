import {
  Entity, PrimaryGeneratedColumn, Column, ManyToOne, Index, CreateDateColumn,
} from "typeorm";
import { User } from "./User";

@Entity({ name: "post" })
export class Post {
  // A generated UUID primary key.
  @PrimaryGeneratedColumn("uuid")
  id!: string;

  @Index()
  @Column({ type: "text" })
  title!: string;

  @Column({ type: "text", nullable: true })
  body!: string | null;

  @Column({ type: "jsonb", default: { tags: [] } })
  meta!: { tags: string[] };

  @Column({ type: "boolean", default: false })
  published!: boolean;

  @ManyToOne(() => User, (user) => user.posts, { onDelete: "CASCADE" })
  author!: User;

  @CreateDateColumn({ type: "timestamptz" })
  createdAt!: Date;
}
